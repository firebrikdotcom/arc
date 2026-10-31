package {{application-id}}.{{module}}

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.test.ext.junit.runners.AndroidJUnit4
import {{application-id}}.MainActivity
import kotlinx.coroutines.runBlocking
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import uniffi.{{crate-name}}.NativeCore
import java.io.File

/** Uses the packaged native library and the real app.sqlite; no fake repository. */
@RunWith(AndroidJUnit4::class)
class {{Type}}FlowTest {
    @get:Rule val compose = createAndroidComposeRule<MainActivity>()
    private fun ready() {
        compose.waitUntil(15000) { compose.onAllNodesWithTag("{{module}}-busy").fetchSemanticsNodes().isEmpty() }
    }
    @Test fun nativeCrudNavigationRecreationAndPersistence() {
        val unique = "{{Type}}-${System.nanoTime()}"
        var createdId: String? = null
        try {
            compose.onNodeWithText("{{Type}}", useUnmergedTree = true).performClick()
            ready()
            compose.onNodeWithTag("{{module}}-new").performClick()
            compose.onNodeWithTag("{{module}}-save").performClick()
            ready()
            compose.onNodeWithTag("{{module}}-error").assertExists()
            compose.onNodeWithTag("{{module}}-name").performTextInput(unique)
            compose.activityRule.scenario.recreate()
            ready()
            compose.onNodeWithTag("{{module}}-name").assertTextContains(unique)
            compose.onNodeWithTag("{{module}}-save").performClick()
            ready()
            compose.onNodeWithTag("{{module}}-detail").assertTextEquals(unique)
            compose.onNodeWithTag("{{module}}-notice").assertTextEquals("Saved")
            val path = File(compose.activity.filesDir, "app.sqlite").canonicalPath
            val core = runBlocking { NativeCore.open(path) }
            createdId = runBlocking { core.list{{pluralMethod}}().first { it.name == unique }.id }
            compose.onNodeWithTag("{{module}}-edit").performClick()
            compose.onNodeWithTag("{{module}}-name").performTextReplacement("$unique edited")
            compose.onNodeWithTag("{{module}}-save").performClick()
            ready()
            compose.activityRule.scenario.recreate()
            ready()
            compose.onNodeWithTag("{{module}}-detail").assertTextEquals("$unique edited")
            check(runBlocking { core.get{{Type}}(createdId!!).version } == 2L)
            compose.onNodeWithTag("{{module}}-delete").performClick()
            compose.onNodeWithTag("{{module}}-cancel-delete").performClick()
            compose.onNodeWithTag("{{module}}-detail").assertExists()
            compose.onNodeWithTag("{{module}}-delete").performClick()
            compose.onNodeWithTag("{{module}}-confirm-delete").performClick()
            ready()
            check(runBlocking { core.list{{pluralMethod}}().none { it.id == createdId } })
            createdId = null
            compose.onNodeWithText("Back").performClick()
            compose.onNodeWithText("{{Type}}").assertExists()
        } finally {
            // Clean up only the record this test created, including failed runs.
            runBlocking {
                val core = NativeCore.open(File(compose.activity.filesDir, "app.sqlite").canonicalPath)
                core.list{{pluralMethod}}().filter { it.id == createdId || it.name == unique || it.name == "$unique edited" }
                    .forEach { core.delete{{Type}}(it.id) }
            }
        }
    }
}
