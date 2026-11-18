plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    buildFeatures { buildConfig = true }
    // one debug key kept in the repository, so every build is signed alike and installs over the last one (a fresh key per CI run would make each update a conflict)
    signingConfigs { getByName("debug") { storeFile = file("debug.keystore"); storePassword = "android"; keyAlias = "androiddebugkey"; keyPassword = "android" } }
    namespace = "org.nx2a.nativeport"
    compileSdk = 36

    defaultConfig {
        applicationId = "unknownnone.nx2a.native" // the source package stays org.nx2a.nativeport (`native` is a Java keyword, so it cannot be a package name)
        buildConfigField("String", "COMMIT", "\"${System.getenv("GITHUB_SHA")?.take(7) ?: "dev"}\"")
        minSdk = 29
        targetSdk = 36
        versionCode = 1
        versionName = "0.1.0"
        ndk { abiFilters += "arm64-v8a" }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            signingConfig = signingConfigs.getByName("debug") // the one key kept in the repository (see above); a private release key is not set up yet
            isDebuggable = false
        }
    }
    lint { checkReleaseBuilds = false; abortOnError = false }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions { jvmTarget = "17" }
    sourceSets { getByName("main") { java.srcDir("src/upstream-java") } } // upstream's Java wrapper, copied by tools/build_android.sh
    packaging { jniLibs { useLegacyPackaging = true } } // extracted: the translator is an executable run from nativeLibraryDir
}
// libnxcore.so comes from `cargo ndk -t arm64-v8a -o app/src/main/jniLibs build --release -p nx-jni` (tools/build_android.sh); the game profiles from tools/fetch_profiles.sh.
