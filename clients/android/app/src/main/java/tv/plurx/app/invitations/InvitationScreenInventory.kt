package tv.plurx.app.invitations

import tv.plurx.app.remote.RemoteSecretStorage

internal object InvitationScreenInventory {
    fun ids(choices: List<InvitationChoice>, consents: List<InvitationConsent>, grants: List<RemoteSecretStorage.Grant>): List<String> {
        require(choices.size <= 160 && consents.size <= 160 && grants.size <= 20)
        // Missing/revoked grants never hide a saved screen's OFF control.
        return (choices.map { it.receiver } + consents.map { it.receiver_id } + grants.map { it.receiverId }).distinct()
    }
}
