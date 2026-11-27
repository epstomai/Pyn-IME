plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

val generatedModelsDir = layout.buildDirectory.dir("generated/modelAssets")

// The APK ships the slim mobile model (pruned + 8-bit quantized, IMEMOD02) as `ime_model.bin`.
// Generate it with `tools/android/build_install.py` (runs `ime-slim` when the full model changes).
val syncModelAssets by tasks.registering(Sync::class) {
    val mobileModel = rootProject.file("../models/ime_model_mobile.bin")
    doFirst {
        check(mobileModel.isFile) {
            "Missing ${mobileModel.path}; run tools/android/build_install.py (or ime-slim) first"
        }
    }
    from(rootProject.file("../models")) {
        include("ime_model_mobile.bin", "reranker.bin")
        rename("ime_model_mobile.bin", "ime_model.bin")
    }
    into(generatedModelsDir.map { it.dir("models") })
}

android {
    namespace = "com.pyn.ime"
    compileSdk = 36

    defaultConfig {
        applicationId = "com.pyn.ime"
        minSdk = 26
        targetSdk = 35
        versionCode = 1
        versionName = "0.1.0"

        ndk {
            abiFilters += listOf("arm64-v8a")
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro"
            )
        }
        debug {
            isMinifyEnabled = false
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    kotlinOptions {
        jvmTarget = "17"
    }

    androidResources {
        noCompress += listOf("bin")
    }

    sourceSets {
        getByName("main") {
            assets.srcDir(generatedModelsDir)
        }
    }
}

tasks.named("preBuild") {
    dependsOn(syncModelAssets)
}

dependencies {
    implementation("androidx.core:core-ktx:1.13.1")
}
