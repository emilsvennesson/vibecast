//! Device-side redirect over `adb` (rooted Android, Magisk `su`).
//!
//! Redirects the device's HTTP/HTTPS egress to the Mac-side MITM with
//! `iptables`, blocking QUIC so everything falls back to interceptable
//! TCP/TLS. All changes are device-wide (every app) and are removed on
//! teardown, which also runs from `Drop` as a safety net.
//!
//! Certificate trust is deliberately NOT handled here: the operator installs a
//! MITM CA into the system store once (e.g. a Magisk cert module), trusted from
//! boot — so no per-session force-stop or cert mount is needed, which keeps
//! Cast discovery intact.

use std::process::Command;

use crate::error::CaptureError;

/// Handle to a device, plus the redirect parameters needed for teardown.
pub struct Adb {
    serial: Option<String>,
    redirect: Option<Redirect>,
}

#[derive(Clone)]
struct Redirect {
    mac_ip: String,
    https_port: u16,
    http_port: u16,
}

impl Adb {
    /// Target a device (optionally by serial, for multi-device hosts).
    #[must_use]
    pub fn new(serial: Option<String>) -> Self {
        Self {
            serial,
            redirect: None,
        }
    }

    /// Verify the device is reachable and `su` grants root.
    pub fn check_root(&self) -> Result<(), CaptureError> {
        let state = self.run(&["get-state"])?;
        if state.trim() != "device" {
            return Err(CaptureError::Adb(format!(
                "device not ready (state: {})",
                state.trim()
            )));
        }
        let id = self.su("id -u")?;
        if id.trim() != "0" {
            return Err(CaptureError::Adb(format!(
                "su did not grant root (uid: {})",
                id.trim()
            )));
        }
        Ok(())
    }

    /// Redirect device HTTP/HTTPS egress to the Mac MITM and block QUIC
    /// (device-wide, every app; loopback is excluded).
    pub fn apply_redirect(
        &mut self,
        mac_ip: &str,
        https_port: u16,
        http_port: u16,
    ) -> Result<(), CaptureError> {
        let redirect = Redirect {
            mac_ip: mac_ip.to_owned(),
            https_port,
            http_port,
        };
        // Store before mutating the device so teardown removes partial rules.
        self.redirect = Some(redirect.clone());
        // Idempotent: clear any stale rules from a prior crashed run first.
        let _ = self.su(&rules("-D", &redirect));
        self.su(&rules("-A", &redirect))?;
        Ok(())
    }

    /// Remove the iptables redirect (best effort; logs on failure).
    pub fn teardown(&mut self) {
        if let Some(redirect) = self.redirect.take() {
            if let Err(error) = self.su(&rules("-D", &redirect)) {
                tracing::warn!(%error, "failed to remove iptables redirect");
            }
        }
    }

    fn run(&self, args: &[&str]) -> Result<String, CaptureError> {
        let mut cmd = Command::new("adb");
        if let Some(serial) = &self.serial {
            cmd.arg("-s").arg(serial);
        }
        cmd.args(args);
        let output = cmd
            .output()
            .map_err(|e| CaptureError::Adb(format!("spawning adb: {e}")))?;
        if !output.status.success() {
            return Err(CaptureError::Adb(format!(
                "adb {} failed: {}",
                args.first().copied().unwrap_or(""),
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Run a script as root via Magisk `su -c`.
    fn su(&self, script: &str) -> Result<String, CaptureError> {
        self.run(&["shell", &format!("su -c {}", shell_quote(script))])
    }
}

impl Drop for Adb {
    fn drop(&mut self) {
        if self.redirect.is_some() {
            self.teardown();
        }
    }
}

/// The iptables/ip6tables rules, parameterised by action (`-A` add, `-D` delete).
/// Joined with `;` so a single `su` invocation applies them all. Device-wide;
/// loopback is excluded from the DNAT so on-device connections keep working.
fn rules(action: &str, r: &Redirect) -> String {
    let dst_https = format!("{}:{}", r.mac_ip, r.https_port);
    let dst_http = format!("{}:{}", r.mac_ip, r.http_port);

    let mut cmds = Vec::new();
    // Block QUIC (v4+v6) and IPv6 80/443 so traffic falls back to IPv4 TCP.
    cmds.push(format!(
        "iptables {action} OUTPUT -p udp --dport 443 -j REJECT"
    ));
    cmds.push(format!(
        "ip6tables {action} OUTPUT -p udp --dport 443 -j REJECT"
    ));
    cmds.push(format!(
        "ip6tables {action} OUTPUT -p tcp --dport 443 -j REJECT"
    ));
    cmds.push(format!(
        "ip6tables {action} OUTPUT -p tcp --dport 80 -j REJECT"
    ));
    // Never redirect loopback (RETURN before DNAT).
    cmds.push(format!(
        "iptables -t nat {action} OUTPUT -p tcp --dport 443 -d 127.0.0.0/8 -j RETURN"
    ));
    cmds.push(format!(
        "iptables -t nat {action} OUTPUT -p tcp --dport 80 -d 127.0.0.0/8 -j RETURN"
    ));
    // Transparent redirect of IPv4 HTTP/HTTPS to the Mac MITM.
    cmds.push(format!(
        "iptables -t nat {action} OUTPUT -p tcp --dport 443 -j DNAT --to-destination {dst_https}"
    ));
    cmds.push(format!(
        "iptables -t nat {action} OUTPUT -p tcp --dport 80 -j DNAT --to-destination {dst_http}"
    ));
    cmds.join("; ")
}

/// Wrap a string in single quotes for safe transport through `adb shell`.
fn shell_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quote_escapes_single_quotes() {
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn rule_actions_are_symmetric_and_device_wide() {
        let r = Redirect {
            mac_ip: "192.168.2.11".into(),
            https_port: 9443,
            http_port: 9080,
        };
        let add = rules("-A", &r);
        let del = rules("-D", &r);
        // Device-wide: no per-UID owner match.
        assert!(!add.contains("--uid-owner"));
        // QUIC blocked and HTTP/HTTPS redirected.
        assert!(add.contains("OUTPUT -p udp --dport 443 -j REJECT"));
        assert!(add.contains("--to-destination 192.168.2.11:9443"));
        assert!(add.contains("--to-destination 192.168.2.11:9080"));
        // Loopback is excluded from the redirect.
        assert!(add.contains("--dport 443 -d 127.0.0.0/8 -j RETURN"));
        assert!(add.contains("--dport 80 -d 127.0.0.0/8 -j RETURN"));
        // Delete mirrors add exactly, swapping only the action.
        assert_eq!(add.replace("-A", "-D"), del);
    }
}
