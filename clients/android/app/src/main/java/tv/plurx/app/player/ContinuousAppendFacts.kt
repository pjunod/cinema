package tv.plurx.app.player

import kotlinx.coroutines.CancellationException
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject

/** Which completed appends are still owed to the ledger. An append names its
 * owners as they were when its load was authorized; by the time the pump runs
 * an owner can be gone, can have stopped reserving the interval, or can have
 * disposed its artifact (a seek forward retires it before the append is
 * reported). Each of those would be refused by the owner on every pass, so
 * only owners that still reserve the exact interval, have not disposed its
 * artifact, and have not already recorded it appended are owed the fact. */
internal object ContinuousAppendFacts {
    data class Fact(val transaction: JsonObject, val interval: JsonObject)

    fun owed(append: ContinuousQueueOwnership.Appended, transactions: List<JsonObject>): List<Fact> {
        if (!append.video) return emptyList()
        return append.transactions.mapNotNull { id ->
            val tx = transactions.singleOrNull { it.text("transaction_id") == id } ?: return@mapNotNull null
            val artifact = append.interval["artifact_id"]
            val reserved = tx["reserved"]?.jsonArray.orEmpty().any { it.jsonObject == append.interval }
            val disposed = artifact != null && tx["disposed"]?.jsonArray.orEmpty().contains(artifact)
            val appended = tx["appended"]?.jsonArray.orEmpty().any { it.jsonObject == append.interval }
            if (reserved && !disposed && !appended) Fact(tx, append.interval) else null
        }
    }
}

/** One pump pass is a sequence of independent fact categories. A category
 * that fails (a refused or unreachable owner) must not stop later ones from
 * running — presentation, retirement and the deadlines are what release pins
 * and recover playback. The first failure is rethrown once every stage ran, so
 * the caller still reports it and backs off; cancellation is never absorbed. */
internal class ContinuousFlushStages {
    private var failure: Exception? = null

    suspend fun stage(action: suspend () -> Unit) {
        try { action() } catch (error: CancellationException) { throw error }
        catch (error: Exception) { if (failure == null) failure = error }
    }

    fun finish() { failure?.let { failure = null; throw it } }
}
