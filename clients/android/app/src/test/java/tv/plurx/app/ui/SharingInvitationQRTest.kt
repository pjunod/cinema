package tv.plurx.app.ui

import com.google.zxing.BinaryBitmap
import com.google.zxing.RGBLuminanceSource
import com.google.zxing.common.HybridBinarizer
import com.google.zxing.qrcode.QRCodeReader
import java.awt.image.BufferedImage
import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import javax.imageio.ImageIO
import org.junit.Assert.*
import org.junit.Test

class SharingInvitationQRTest {
    @Test fun invitationPngRoundTripPreservesTheExactSecretAtTypicalAndMaximumSizes() {
        for (size in listOf(800, SHARING_QR_MAX_BYTES)) {
            val prefix = "cinema-share-v1:"
            val invitation = prefix + "a".repeat(size - prefix.length)
            val matrix = sharingInvitationQR(invitation)
            val image = BufferedImage(matrix.width, matrix.height, BufferedImage.TYPE_INT_RGB)
            for (y in 0 until matrix.height) for (x in 0 until matrix.width) image.setRGB(x, y, if (matrix[x, y]) 0 else 0xFFFFFF)
            val png = ByteArrayOutputStream().also { assertTrue(ImageIO.write(image, "png", it)) }.toByteArray()
            val decodedImage = ImageIO.read(ByteArrayInputStream(png))
            val pixels = decodedImage.getRGB(0, 0, decodedImage.width, decodedImage.height, null, 0, decodedImage.width)
            val source = RGBLuminanceSource(decodedImage.width, decodedImage.height, pixels)
            assertEquals(invitation, QRCodeReader().decode(BinaryBitmap(HybridBinarizer(source))).text)
        }
    }
    @Test fun oversizedAndUnrelatedQrMaterialIsRefusedWithTextFallbackAvailable() {
        for (invalid in listOf("https://example.test", "cinema-share-v1:" + "a".repeat(SHARING_QR_MAX_BYTES), "cinema-share-v1:abc\n")) {
            assertNotNull(runCatching { sharingInvitationQR(invalid) }.exceptionOrNull())
        }
    }
}
