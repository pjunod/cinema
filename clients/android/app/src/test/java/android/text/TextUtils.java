package android.text;

/**
 * Host-only framework string operations used by Media3's real track models.
 *
 * The Android unit-test jar throws for these methods. Keep their Android
 * CharSequence semantics here instead of enabling returnDefaultValues, which
 * would hide framework calls and change Media3's language normalization.
 * This class is confined to src/test; it is never packaged in either APK.
 * Semantics follow the installed Android SDK 37 source, android/text/TextUtils.java,
 * from AOSP frameworks/base/core/java/android/text/TextUtils.java (isEmpty/equals).
 */
public final class TextUtils {
    private TextUtils() {}

    public static boolean isEmpty(CharSequence value) {
        return value == null || value.length() == 0;
    }

    public static boolean equals(CharSequence first, CharSequence second) {
        if (first == second) {
            return true;
        }
        if (first == null || second == null || first.length() != second.length()) {
            return false;
        }
        for (int i = 0; i < first.length(); i++) {
            if (first.charAt(i) != second.charAt(i)) {
                return false;
            }
        }
        return true;
    }
}
