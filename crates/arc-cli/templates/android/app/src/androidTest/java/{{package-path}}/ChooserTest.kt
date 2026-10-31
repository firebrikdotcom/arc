package {{application-id}}

import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.assertIsDisplayed
import org.junit.Rule
import org.junit.Test

class ChooserTest {
    @get:Rule val compose = createAndroidComposeRule<MainActivity>()
    @Test fun chooserShowsApplicationName() {
        compose.onNodeWithText("{{project-name}}").assertIsDisplayed()
    }
}
