package {{application-id}}

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.activity.compose.BackHandler
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
// arc:catalog-imports

class ResourceEntry(val id: String, val label: String, val screen: @Composable (() -> Unit) -> Unit)

val resourceCatalog: List<ResourceEntry> = listOf(
    // arc:catalog-entries
)

@Composable
fun ResourceChooser() {
    var selected by rememberSaveable { mutableStateOf<String?>(null) }
    val entry = resourceCatalog.firstOrNull { it.id == selected }
    BackHandler(enabled = entry != null) { selected = null }
    if (entry != null) {
        entry.screen { selected = null }
    } else {
        Column(Modifier.padding(24.dp)) {
            Text("{{project-name}}", style = androidx.compose.material3.MaterialTheme.typography.headlineMedium)
            if (resourceCatalog.isEmpty()) Text("No resources yet. Generate your first resource with arc generate resource Note --ffi.")
            resourceCatalog.forEach { resource ->
                Button(onClick = { selected = resource.id }) { Text(resource.label) }
            }
        }
    }
}
