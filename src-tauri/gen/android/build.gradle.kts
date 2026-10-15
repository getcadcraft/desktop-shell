buildscript {
    repositories {
        google()
        mavenCentral()
    }
    dependencies {
        classpath("com.android.tools.build:gradle:9.3.1")
        classpath("org.jetbrains.kotlin:kotlin-gradle-plugin:2.2.10")
    }
}

allprojects {
    repositories {
        google()
        mavenCentral()
        // The Kotlin half of rustls-platform-verifier (TLS checked by Android's own trust manager), vendored in the repo:
        // desktop-shell/android/maven (see android/README.md), never fetched while building.
        maven { url = uri(rootDir.resolve("../../../android/maven")) }
    }
}

tasks.register("clean").configure {
    delete("build")
}

