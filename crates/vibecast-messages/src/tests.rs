//! Parsing and serialization parity tests for the Cast JSON models.

use serde_json::{json, Value};

use crate::*;

#[test]
fn parses_connect_via_discriminated_union() {
    let msg: ConnectionMessage =
        serde_json::from_value(json!({"type": "CONNECT", "userAgent": "chrome"})).unwrap();
    assert!(matches!(msg, ConnectionMessage::Connect(_)));

    let msg: ConnectionMessage =
        serde_json::from_value(json!({"type": "CLOSE", "reasonCode": 5})).unwrap();
    assert!(matches!(msg, ConnectionMessage::Close(_)));
}

#[test]
fn parses_receiver_requests_and_ignores_unknown_fields() {
    let req: ReceiverRequest =
        serde_json::from_value(json!({"type": "GET_STATUS", "requestId": 7, "extra": true}))
            .unwrap();
    match req {
        ReceiverRequest::GetStatus(r) => assert_eq!(r.request_id, 7),
        _ => panic!("expected GET_STATUS"),
    }
}

#[test]
fn launch_resolves_nested_credentials() {
    let req: LaunchRequest = serde_json::from_value(json!({
        "requestId": 1,
        "appId": "95370A1C",
        "appParams": {"launchCheckerParams": {"credentialsData": {
            "credentials": "tok", "credentialsType": "svt"
        }}}
    }))
    .unwrap();
    let (creds, kind) = req.resolved_credentials();
    assert_eq!(creds.as_deref(), Some("tok"));
    assert_eq!(kind.as_deref(), Some("svt"));
}

#[test]
fn launch_top_level_credentials_take_precedence() {
    let req: LaunchRequest = serde_json::from_value(json!({
        "requestId": 1, "appId": "X", "credentials": "top",
        "appParams": {"launchCheckerParams": {"credentialsData": {"credentials": "nested"}}}
    }))
    .unwrap();
    assert_eq!(req.resolved_credentials().0.as_deref(), Some("top"));
}

#[test]
fn set_volume_applies_only_provided_fields() {
    let req: SetVolumeRequest = serde_json::from_value(
        json!({"type": "SET_VOLUME", "requestId": 2, "volume": {"muted": true}}),
    )
    .unwrap();
    let mut volume = Volume {
        level: 0.8,
        muted: false,
        control_type: Some("attenuation".into()),
        step_interval: Some(0.05),
    };
    volume.apply_update(&req.volume);
    // Only `muted` changed; level and control_type preserved.
    assert!(volume.muted);
    assert_eq!(volume.level, 0.8);
    assert_eq!(volume.control_type.as_deref(), Some("attenuation"));
    // Presence tracking: omitted fields are Missing, provided ones are Set.
    assert!(!req.volume.level.is_set());
    assert!(req.volume.muted.is_set());
}

#[test]
fn set_volume_rejects_explicit_null_for_non_nullable_field() {
    // An explicit `null` for a non-nullable field is a malformed update, not
    // an omission: it must fail to parse rather than be silently ignored.
    let result: Result<SetVolumeRequest, _> = serde_json::from_value(
        json!({"type": "SET_VOLUME", "requestId": 2, "volume": {"level": null}}),
    );
    assert!(result.is_err());
}

#[test]
fn receiver_status_serializes_camel_case() {
    let status = ReceiverStatus {
        applications: vec![],
        volume: Volume {
            level: 1.0,
            muted: false,
            control_type: Some("attenuation".into()),
            step_interval: Some(0.05),
        },
        is_active_input: Some(true),
        is_stand_by: Some(false),
    };
    let value = serde_json::to_value(ReceiverStatusResponse::new(9, status)).unwrap();
    assert_eq!(value["type"], "RECEIVER_STATUS");
    assert_eq!(value["requestId"], 9);
    assert_eq!(value["status"]["volume"]["controlType"], "attenuation");
    assert_eq!(value["status"]["isActiveInput"], true);
    assert_eq!(value["status"]["applications"], json!([]));
}

#[test]
fn application_status_omits_none_fields() {
    let app = ApplicationStatus {
        app_id: "A".into(),
        display_name: "SVT".into(),
        session_id: "s".into(),
        transport_id: "t".into(),
        status_text: "SVT".into(),
        namespaces: vec![CastNamespace {
            name: "urn:x".into(),
        }],
        is_idle_screen: false,
        app_type: Some("WEB".into()),
        icon_url: None,
        launched_from_cloud: Some(false),
        sender_connected: Some(true),
        universal_app_id: Some("A".into()),
    };
    let value = serde_json::to_value(&app).unwrap();
    let object = value.as_object().unwrap();
    assert!(!object.contains_key("iconUrl")); // None omitted
    assert_eq!(value["appId"], "A");
    assert_eq!(value["namespaces"][0]["name"], "urn:x");
    assert_eq!(value["senderConnected"], true);
}

#[test]
fn app_availability_marks_all_available() {
    let value = serde_json::to_value(AppAvailabilityResponse::available(
        3,
        &["A".into(), "B".into()],
    ))
    .unwrap();
    assert_eq!(value["type"], "GET_APP_AVAILABILITY");
    assert_eq!(value["availability"]["A"], "APP_AVAILABLE");
    assert_eq!(value["availability"]["B"], "APP_AVAILABLE");
}

#[test]
fn device_info_response_has_defaults_and_camel_case() {
    let value = serde_json::to_value(DeviceInfoResponse::new(
        4,
        "dev-id".into(),
        "Chromecast".into(),
        "Living Room".into(),
    ))
    .unwrap();
    assert_eq!(value["type"], "DEVICE_INFO");
    assert_eq!(value["deviceId"], "dev-id");
    assert_eq!(value["deviceCapabilities"], 4101);
    assert_eq!(value["controlNotifications"], 1);
}

#[test]
fn setup_uses_snake_case_and_accepts_both_request_id_forms() {
    let a: SetupRequest = serde_json::from_value(json!({"requestId": 5})).unwrap();
    let b: SetupRequest = serde_json::from_value(json!({"request_id": 5})).unwrap();
    assert_eq!(a.request_id, 5);
    assert_eq!(b.request_id, 5);

    let value =
        serde_json::to_value(SetupResponse::ok(5, "Living Room".into(), "udn".into())).unwrap();
    assert_eq!(value["type"], "eureka_info");
    assert_eq!(value["response_code"], 200);
    assert_eq!(value["response_string"], "OK");
    assert_eq!(value["data"]["device_info"]["ssdp_udn"], "udn");
    assert_eq!(value["data"]["version"], 8);
}

#[test]
fn extract_request_id_handles_both_casings() {
    assert_eq!(extract_request_id(&json!({"requestId": 11})), 11);
    assert_eq!(extract_request_id(&json!({"request_id": 12})), 12);
    assert_eq!(extract_request_id(&Value::Null), 0);
}
