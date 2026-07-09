import org.gradle.api.tasks.Exec

plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.android)
    alias(libs.plugins.ktlint)
    alias(libs.plugins.detekt)
}

// ---------------------------------------------------------------------------
// Rust integration: cargo-ndk builds the vibecast-ffi cdylib per ABI, and
// uniffi-bindgen generates the Kotlin bindings from the compiled library.
// Both run from the Cargo workspace root (the repo, one level above android/).
// ---------------------------------------------------------------------------
val workspaceDir: File = rootProject.projectDir.parentFile
val cargoBinDir = File(System.getProperty("user.home"), ".cargo/bin")
val cargo: String = File(cargoBinDir, "cargo").absolutePath
val androidApiLevel = 24
val rustAbis = listOf("arm64-v8a", "x86_64")

// Single source of truth for the app version; release-please bumps the literal
// below on release (see release-please-config.json extra-files).
val appVersionName = "0.1.1" // x-release-please-version

// Derive a monotonic versionCode from the semver name
// (MAJOR*10000 + MINOR*100 + PATCH), ignoring any prerelease suffix. Floored at
// 1 so pipeline test builds (e.g. 0.0.0-test.N) still yield a valid code.
fun versionCodeFrom(name: String): Int {
    val (major, minor, patch) =
        name
            .substringBefore('-')
            .split('.')
            .map { it.toIntOrNull() ?: 0 }
            .let { Triple(it.getOrElse(0) { 0 }, it.getOrElse(1) { 0 }, it.getOrElse(2) { 0 }) }
    return maxOf(1, major * 10000 + minor * 100 + patch)
}

val rustJniLibsDir = layout.buildDirectory.dir("rustJniLibs")
val uniffiGenDir = layout.buildDirectory.dir("generated/uniffi")

/** NDK path from env, else the newest NDK under the SDK. Resolved at exec time. */
fun resolveNdkHome(): String =
    System.getenv("ANDROID_NDK_HOME")
        ?: File(android.sdkDirectory, "ndk")
            .listFiles()
            ?.filter { it.isDirectory }
            ?.maxByOrNull { it.name }
            ?.absolutePath
        ?: throw GradleException("NDK not found: set ANDROID_NDK_HOME or install one via sdkmanager")

fun Exec.applyCargoEnvironment() {
    environment("ANDROID_NDK_HOME", resolveNdkHome())
    environment("ANDROID_HOME", android.sdkDirectory.absolutePath)
    environment("PATH", "${cargoBinDir.absolutePath}${File.pathSeparator}${System.getenv("PATH")}")
}

val cargoBuildAndroid by tasks.registering(Exec::class) {
    group = "rust"
    description = "Cross-compile vibecast-ffi (release) for ${rustAbis.joinToString()} via cargo-ndk"
    workingDir = workspaceDir
    val self = this
    doFirst { self.applyCargoEnvironment() }
    val cmd = mutableListOf(cargo, "ndk")
    rustAbis.forEach { cmd += listOf("-t", it) }
    cmd +=
        listOf(
            "-P",
            androidApiLevel.toString(),
            "-o",
            rustJniLibsDir.get().asFile.absolutePath,
            "build",
            "--release",
            "-p",
            "vibecast-ffi",
        )
    commandLine(cmd)
}

// Build an *unstripped host* library for binding generation: the shipped
// Android .so is stripped (release profile), which drops the UniFFI metadata
// that `uniffi-bindgen --library` reads. Bindings are target-agnostic, so the
// host library is equivalent and always has the metadata.
val cargoBuildHostFfi by tasks.registering(Exec::class) {
    group = "rust"
    description = "Build vibecast-ffi for the host (unstripped) for uniffi-bindgen"
    workingDir = workspaceDir
    val self = this
    doFirst { self.applyCargoEnvironment() }
    commandLine(cargo, "build", "-p", "vibecast-ffi")
}

val generateUniffiKotlin by tasks.registering(Exec::class) {
    group = "rust"
    description = "Generate Kotlin bindings from the host vibecast-ffi library"
    dependsOn(cargoBuildHostFfi)
    workingDir = workspaceDir
    val self = this
    // The library path/extension is host-dependent, so resolve it at exec time.
    commandLine("true")
    doFirst {
        self.applyCargoEnvironment()
        uniffiGenDir.get().asFile.mkdirs()
        val debugDir = File(workspaceDir, "target/debug")
        val lib =
            listOf("libvibecast_ffi.dylib", "libvibecast_ffi.so", "vibecast_ffi.dll")
                .map { File(debugDir, it) }
                .firstOrNull { it.exists() }
                ?: throw GradleException("host vibecast-ffi library not found in $debugDir")
        self.commandLine(
            cargo,
            "run",
            "-q",
            "-p",
            "uniffi-bindgen",
            "--",
            "generate",
            "--library",
            lib.absolutePath,
            "--language",
            "kotlin",
            "--out-dir",
            uniffiGenDir.get().asFile.absolutePath,
            "--no-format",
        )
    }
}

android {
    namespace = "com.vibecast.receiver"
    compileSdk = 36
    buildToolsVersion = "36.0.0"

    defaultConfig {
        applicationId = "com.vibecast.receiver"
        minSdk = 26
        targetSdk = 36
        versionCode = versionCodeFrom(appVersionName)
        versionName = appVersionName
        ndk { abiFilters += rustAbis }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions {
        jvmTarget = "17"
    }

    // Release signing is wired only when the keystore env vars are present (CI
    // release builds inject them from secrets). Local/dev release builds fall
    // back to no signing config → an unsigned APK, which is fine for testing.
    val keystoreFile: String? = System.getenv("ANDROID_KEYSTORE_FILE")
    signingConfigs {
        if (keystoreFile != null) {
            create("release") {
                storeFile = file(keystoreFile)
                storePassword = System.getenv("ANDROID_KEYSTORE_PASSWORD")
                keyAlias = System.getenv("ANDROID_KEY_ALIAS")
                keyPassword = System.getenv("ANDROID_KEY_PASSWORD")
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
            signingConfig = signingConfigs.findByName("release")
        }
    }

    // 16 KB page compatibility: never legacy-package (extract) .so files.
    packaging {
        jniLibs {
            useLegacyPackaging = false
        }
    }

    sourceSets["main"].jniLibs.srcDir(rustJniLibsDir)
    sourceSets["main"].kotlin.srcDir(uniffiGenDir)

    lint {
        warningsAsErrors = true
        abortOnError = true
        // Advisory/environmental checks — dependency + tooling freshness, ChromeOS
        // ABI hints, backup-rules boilerplate — are noise for a native LAN server
        // and would break CI on every upstream release. Real code-quality checks
        // stay on; NewApi for the generated UniFFI cleaner is scoped in lint.xml.
        //
        // OldTargetApi: targetSdk 36 is the max AGP 8.13 supports (37 needs
        // ACCESS_LOCAL_NETWORK). This check compares against the highest platform
        // *installed*, so it stays quiet locally but fires on CI runners that
        // ship a newer platform. Bump targetSdk when moving to a newer AGP.
        disable +=
            setOf(
                "GradleDependency",
                "AndroidGradlePluginVersion",
                "UseTomlInstead",
                "VectorRaster",
                "ChromeOsAbiSupport",
                "DataExtractionRules",
                "OldTargetApi",
            )
    }
}

// Native libs (jniLibs) + generated bindings must exist before compile/merge.
tasks.named("preBuild") { dependsOn(cargoBuildAndroid, generateUniffiKotlin) }

ktlint {
    version.set("1.3.1")
    android.set(true)
    // Never lint the generated UniFFI bindings.
    filter {
        exclude { element -> element.file.path.contains("generated/uniffi") }
    }
}

detekt {
    buildUponDefaultConfig = true
    config.setFrom(rootProject.file("config/detekt/detekt.yml"))
    // Detekt only inspects our sources, not the generated bindings.
    source.setFrom(files("src/main/kotlin"))
}

dependencies {
    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.appcompat)
    // JNA (@aar bundles the native dispatch libs per ABI) — required by the
    // UniFFI-generated Kotlin bindings.
    implementation("net.java.dev.jna:jna:${libs.versions.jna.get()}@aar")
}
