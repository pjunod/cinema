package tv.plurx.app.invitations

/** Token callbacks are installation-wide; completion belongs to each current screen binding. */
internal class InvitationRotationQueue {
    private var token: String? = null
    private val confirmed = mutableMapOf<String, Pair<String, String>>()
    private val uncertain = mutableSetOf<String>()
    fun offer(value: String) { token = value }
    fun remaining(bindings: Map<String, String>): List<Pair<String, String>> {
        require(bindings.size <= 160)
        confirmed.keys.retainAll(bindings.keys)
        val current = token ?: return emptyList()
        return bindings.entries.filter { confirmed[it.key] != (current to it.value) && it.key !in uncertain }.map { it.key to current }
    }
    fun complete(receiver: String, value: String, binding: String) {
        require(confirmed.size < 160 || receiver in confirmed)
        confirmed[receiver] = value to binding; uncertain.remove(receiver)
    }
    fun unknown(receiver: String) { require(uncertain.size < 160 || receiver in uncertain); uncertain.add(receiver) }
    fun explicitRetry(receiver: String) { uncertain.remove(receiver) }
    fun clearAuthority() { confirmed.clear(); uncertain.clear() }
}
