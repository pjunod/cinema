import XCTest
@testable import plurx

final class SplitHLSAudioTests: XCTestCase {
    func testIdentifiesOnlyUnambiguousAudioFromTheVideoGroup() {
        let video = String(repeating: "a", count: 64)
        let audio = String(repeating: "b", count: 64)
        let master = """
        #EXTM3U
        #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID="audio",NAME="English, Original",DEFAULT=YES,STABLE-RENDITION-ID="a3",URI="audio/\(audio)/index.m3u8"
        #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID="audio",NAME="French",STABLE-RENDITION-ID="a7",URI="audio/\(video)/index.m3u8"
        #EXT-X-STREAM-INF:BANDWIDTH=800000,CODECS="avc1.4d401f,mp4a.40.2",AUDIO="audio"
        video/\(video)/index.m3u8
        """
        XCTAssertEqual(SplitHLSAudio.names(in: master), [3: "English, Original", 7: "French"])
        XCTAssertEqual(SplitHLSAudio.names(in: master.replacingOccurrences(of: "French", with: "English, Original")), [:])
        XCTAssertEqual(SplitHLSAudio.names(in: master.replacingOccurrences(of: "a7", with: "a3")), [:])
        XCTAssertEqual(SplitHLSAudio.names(in: master.replacingOccurrences(of: "AUDIO=\"audio\"", with: "AUDIO=\"other\"")), [:])
        XCTAssertEqual(SplitHLSAudio.names(in: master.replacingOccurrences(of: "audio/\(audio)", with: "../\(audio)")), [:])
    }
}
