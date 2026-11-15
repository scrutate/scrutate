import org.gradle.api.tasks.testing.logging.TestExceptionFormat
import org.jetbrains.intellij.platform.gradle.TestFrameworkType

plugins {
    java
    id("org.jetbrains.intellij.platform")
}

group = "dev.scrutate"
version = "0"

dependencies {
    testImplementation("junit:junit:4.13.2")

    intellijPlatform {
        intellijIdea("2026.3")
        // the LSP4IJ release from the Marketplace, as a user installs it
        plugin("com.redhat.devtools.lsp4ij:0.21.0")
        testFramework(TestFrameworkType.Platform)
        testBundledPlugins("com.intellij.modules.ultimate")
    }
}

intellijPlatform {
    instrumentCode = false
    buildSearchableOptions = false
}

tasks.test {
    maxHeapSize = "2g"
    systemProperty("idea.log.leaked.projects.in.tests", "false")
    testLogging {
        events("passed", "skipped", "failed")
        exceptionFormat = TestExceptionFormat.FULL
        showStandardStreams = true
    }
}
