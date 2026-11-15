import org.jetbrains.intellij.platform.gradle.extensions.intellijPlatform

// The same plugin and repositories as JetBrains' intellij-platform-plugin-template.
rootProject.name = "scrutate-lsp4ij-test"

plugins {
    id("org.jetbrains.intellij.platform.settings") version "2.20.1"
}

@Suppress("UnstableApiUsage")
dependencyResolutionManagement {
    repositories {
        mavenCentral()
        intellijPlatform {
            defaultRepositories()
        }
    }
}
