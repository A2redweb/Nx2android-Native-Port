plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "org.nx2a.nativeport"
    compileSdk = 36

    defaultConfig {
        applicationId = "org.nx2a.nativeport"
        minSdk = 29
        targetSdk = 36
        versionCode = 1
        versionName = "0.1.0"
        ndk { abiFilters += "arm64-v8a" }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            signingConfig = signingConfigs.getByName("debug") // a release signing key is not set up yet
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions { jvmTarget = "17" }
    sourceSets { getByName("main") { java.srcDir("src/upstream-java") } } // upstream's Java wrapper, copied by tools/build_android.sh
    packaging { jniLibs { useLegacyPackaging = true } } // extracted: the translator is an executable run from nativeLibraryDir
}
// libnxcore.so comes from `cargo ndk -t arm64-v8a -o app/src/main/jniLibs build --release -p nx-jni` (tools/build_android.sh); the game profiles from tools/fetch_profiles.sh.
