package tv.plurx.app.ui

import com.google.zxing.BarcodeFormat
import com.google.zxing.EncodeHintType
import com.google.zxing.common.BitMatrix
import com.google.zxing.qrcode.QRCodeWriter
import com.google.zxing.qrcode.decoder.ErrorCorrectionLevel
import tv.plurx.app.data.SharedSharingValidation

internal const val SHARING_QR_MAX_BYTES = 2331

internal fun sharingInvitationQR(text: String): BitMatrix {
    require(SharedSharingValidation.invitation(text)) { "Invalid Cinema sharing invitation" }
    require(text.toByteArray(Charsets.UTF_8).size <= SHARING_QR_MAX_BYTES) { "Invitation is too large for one QR code" }
    return QRCodeWriter().encode(text, BarcodeFormat.QR_CODE, 768, 768,
        mapOf(EncodeHintType.ERROR_CORRECTION to ErrorCorrectionLevel.M, EncodeHintType.MARGIN to 4))
}
