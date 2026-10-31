package {{application-id}}.{{module}}

import androidx.lifecycle.SavedStateHandle
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import uniffi.{{crate-name}}.{{RecordType}}

data class {{Type}}State(
    val records: List<{{RecordType}}> = emptyList(),
    val selected: {{RecordType}}? = null,
    val mode: String = "list",
    val draft: String = "",
    val busy: Boolean = false,
    val error: String? = null,
    val notice: String? = null,
)

class {{Type}}ViewModel(private val repository: {{Type}}Repository, private val saved: SavedStateHandle) : ViewModel() {
    private val mutable = MutableStateFlow({{Type}}State(
        mode = saved["mode"] ?: "list", draft = saved["draft"] ?: "",
    ))
    val state: StateFlow<{{Type}}State> = mutable.asStateFlow()
    init { refresh() }

    private fun operation(action: suspend () -> Unit) {
        if (mutable.value.busy) return
        mutable.value = mutable.value.copy(busy = true, error = null)
        viewModelScope.launch {
            try { action() }
            catch (cancelled: CancellationException) { throw cancelled }
            catch (failure: Exception) {
                mutable.value = mutable.value.copy(error = failure.message ?: "Operation failed")
            } finally { mutable.value = mutable.value.copy(busy = false) }
        }
    }

    private fun mode(value: String, id: String? = null) {
        saved["mode"] = value
        saved["id"] = id
        mutable.value = mutable.value.copy(mode = value, error = null)
    }
    fun editDraft(value: String) {
        if (mutable.value.busy) return
        saved["draft"] = value
        mutable.value = mutable.value.copy(draft = value, error = null)
    }
    fun create() {
        if (mutable.value.busy) return
        mode("create")
        editDraft("")
    }
    fun edit() {
        if (mutable.value.busy) return
        val selected = mutable.value.selected ?: return
        mode("edit", selected.id)
        editDraft(selected.name)
    }
    fun back() {
        if (mutable.value.busy) return
        if (mutable.value.mode == "edit") {
            mode("detail", saved["id"])
        } else {
            mode("list")
            mutable.value = mutable.value.copy(selected = null)
        }
        refresh()
    }
    fun refresh() = operation {
        val records = repository.list()
        val id: String? = saved["id"]
        val selected = if (id != null) repository.get(id) else null
        mutable.value = mutable.value.copy(records = records, selected = selected)
    }
    fun select(id: String) = operation {
        val selected = repository.get(id)
        mode("detail", id)
        mutable.value = mutable.value.copy(selected = selected, notice = null)
    }
    fun save() {
        if (mutable.value.busy) return
        if (mutable.value.mode != "create" && mutable.value.mode != "edit") return
        val draft = mutable.value.draft
        val id: String? = saved["id"]
        val editing = mutable.value.mode == "edit"
        operation {
            val committedId = if (editing) {
                checkNotNull(id)
                repository.rename(id, draft)
                id
            } else repository.create(draft)
            // Change mode before refreshing: a read failure after commit must not
            // leave a create form offering an accidental duplicate submission.
            mode("detail", committedId)
            mutable.value = mutable.value.copy(notice = "Saved", selected = null)
            val selected = repository.get(committedId)
            val records = repository.list()
            mutable.value = mutable.value.copy(selected = selected, records = records)
        }
    }
    fun delete() {
        val id: String = saved["id"] ?: return
        operation {
            repository.delete(id)
            mode("list")
            mutable.value = mutable.value.copy(selected = null, notice = "Deleted")
            mutable.value = mutable.value.copy(records = repository.list())
        }
    }
}
