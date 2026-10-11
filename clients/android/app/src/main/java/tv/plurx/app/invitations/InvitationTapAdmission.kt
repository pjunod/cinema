package tv.plurx.app.invitations

internal data class InvitationTapPermit(val scope: String, val installation: String, val phoneGeneration: Long,
    val fingerprint: String, val intents: Map<String, Long>) {
    fun permits(state: InvitationLocalState, receiver: String, fingerprint: String, companionOwned: Boolean, corrupt: Boolean): Boolean {
        if (!companionOwned || corrupt || state.pendingDeletion || state.lostProof || state.phoneSecret == null ||
            state.scope != scope || state.installation != installation || state.phone?.phone_generation != phoneGeneration ||
            state.loginFingerprint != this.fingerprint || fingerprint != this.fingerprint) return false
        val choice = state.choices.firstOrNull { it.receiver == receiver } ?: return false
        return choice.enabled && !choice.pendingSync && intents[receiver] == choice.intentRevision
    }
    companion object {
        fun capture(state: InvitationLocalState, id: String, fingerprint: String, companionOwned: Boolean, corrupt: Boolean): InvitationTapPermit {
            require(companionOwned && !corrupt && !state.pendingDeletion && !state.lostProof && state.phoneSecret != null)
            require(state.loginFingerprint == fingerprint && state.installation == InvitationWire.installation(id))
            val phone = requireNotNull(state.phone); require(phone.installation_id == state.installation && state.choices.size <= 160)
            return InvitationTapPermit(state.scope, phone.installation_id, phone.phone_generation, fingerprint, state.choices.associate { it.receiver to it.intentRevision })
        }
    }
}
