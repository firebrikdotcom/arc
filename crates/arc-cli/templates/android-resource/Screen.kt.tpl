package {{application-id}}.{{module}}

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.createSavedStateHandle
import androidx.lifecycle.viewmodel.CreationExtras
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.collectAsStateWithLifecycle

@Composable
fun {{Type}}Screen(onBack: () -> Unit) {
    val context = LocalContext.current.applicationContext
    val factory = remember(context) {
        object : ViewModelProvider.Factory {
            @Suppress("UNCHECKED_CAST")
            override fun <T : ViewModel> create(modelClass: Class<T>, extras: CreationExtras): T =
                {{Type}}ViewModel(Native{{Type}}Repository(context), extras.createSavedStateHandle()) as T
        }
    }
    val model: {{Type}}ViewModel = viewModel(key = "{{module}}", factory = factory)
    val state by model.state.collectAsStateWithLifecycle()
    val owner = LocalLifecycleOwner.current
    DisposableEffect(owner, model) {
        val observer = LifecycleEventObserver { _, event -> if (event == Lifecycle.Event.ON_RESUME) model.refresh() }
        owner.lifecycle.addObserver(observer)
        onDispose { owner.lifecycle.removeObserver(observer) }
    }
    var confirmDelete by rememberSaveable { mutableStateOf(false) }
    val back: () -> Unit = { if (!state.busy) { if (state.mode == "list") onBack() else model.back() } }
    // The chooser also owns Back; the resource handles its nested route first.
    BackHandler { back() }
    Column(Modifier.fillMaxSize().padding(24.dp).testTag("{{module}}-screen")) {
        Text("{{Type}}", style = MaterialTheme.typography.headlineMedium)
        TextButton(onClick = back, enabled = !state.busy) { Text("Back") }
        if (state.busy) CircularProgressIndicator(Modifier.testTag("{{module}}-busy"))
        state.error?.let {
            Text(it, color = MaterialTheme.colorScheme.error,
                modifier = Modifier.testTag("{{module}}-error").semantics { liveRegion = LiveRegionMode.Polite })
        }
        state.notice?.let {
            Text(it, modifier = Modifier.testTag("{{module}}-notice").semantics { liveRegion = LiveRegionMode.Polite })
        }
        when (state.mode) {
            "list" -> {
                Button(onClick = model::create, enabled = !state.busy, modifier = Modifier.testTag("{{module}}-new")) { Text("New {{Type}}") }
                TextButton(onClick = model::refresh, enabled = !state.busy) { Text("Refresh") }
                if (state.records.isEmpty() && !state.busy) Text("No {{view}} yet")
                LazyColumn {
                    items(state.records, key = { it.id }) { record ->
                        TextButton(onClick = { model.select(record.id) }, enabled = !state.busy,
                            modifier = Modifier.testTag("{{module}}-row-${record.id}")) { Text(record.name) }
                    }
                }
            }
            "detail" -> {
                state.selected?.let { record ->
                    Text(record.name, modifier = Modifier.testTag("{{module}}-detail"))
                    Text("Version ${record.version}")
                    Button(onClick = model::edit, enabled = !state.busy, modifier = Modifier.testTag("{{module}}-edit")) { Text("Edit") }
                    TextButton(onClick = { confirmDelete = true }, enabled = !state.busy, modifier = Modifier.testTag("{{module}}-delete")) { Text("Delete") }
                }
                TextButton(onClick = model::refresh, enabled = !state.busy) { Text("Refresh") }
            }
            "create", "edit" -> {
                OutlinedTextField(value = state.draft, onValueChange = model::editDraft,
                    label = { Text("Name") }, enabled = !state.busy, isError = state.error != null,
                    modifier = Modifier.fillMaxWidth().testTag("{{module}}-name"))
                Button(onClick = model::save, enabled = !state.busy, modifier = Modifier.testTag("{{module}}-save")) { Text("Save") }
            }
        }
    }
    if (confirmDelete) AlertDialog(onDismissRequest = { confirmDelete = false },
        title = { Text("Delete {{Type}}?") }, text = { Text("This action cannot be undone.") },
        confirmButton = { TextButton(onClick = { confirmDelete = false; model.delete() }, enabled = !state.busy,
            modifier = Modifier.testTag("{{module}}-confirm-delete")) { Text("Delete") } },
        dismissButton = { TextButton(onClick = { confirmDelete = false }, modifier = Modifier.testTag("{{module}}-cancel-delete")) { Text("Cancel") } })
}
