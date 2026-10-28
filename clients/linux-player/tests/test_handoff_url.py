"""The scheme URL is parsed strictly: a malformed URL is refused before anything is contacted."""

import unittest

import support  # noqa: F401  (puts the package on sys.path)
from plurx_player.handoff import HandoffUrlError, normalize_origin, parse_handoff_url

CODE = "Q2xhdWRlLWhhbmRvZmYtY29kZS0zMi1ieXRlcy1sb25n"


class ParseTests(unittest.TestCase):
    def test_the_web_app_url_parses(self):
        handoff = parse_handoff_url(f"plurx-player://play?server=http%3A%2F%2Fmedia1%3A8096&code={CODE}")
        self.assertEqual(handoff.server, "http://media1:8096")
        self.assertEqual(handoff.code, CODE)

    def test_origins_are_canonical(self):
        self.assertEqual(normalize_origin("HTTPS://Plurx.Example:443"), "https://plurx.example")
        self.assertEqual(normalize_origin("http://plurx.example:80"), "http://plurx.example")
        self.assertEqual(normalize_origin("http://[::1]:8096"), "http://[::1]:8096")
        self.assertEqual(normalize_origin("https://10.42.0.5:8443"), "https://10.42.0.5:8443")

    def test_malformed_urls_are_refused(self):
        server = "http%3A%2F%2Fmedia1%3A8096"
        bad = [
            "", "play?server=x", f"https://play?server={server}&code={CODE}",
            f"plurx-player://open?server={server}&code={CODE}",
            f"plurx-player://play/extra?server={server}&code={CODE}",
            f"plurx-player://play?server={server}", f"plurx-player://play?code={CODE}",
            f"plurx-player://play?server={server}&code={CODE}&token=abc",
            f"plurx-player://play?server={server}&server={server}&code={CODE}",
            f"plurx-player://play?server={server}&code={CODE}#frag",
            f"plurx-player://play?server={server}&code=short",
            f"plurx-player://play?server={server}&code={CODE}%2F",
            f"plurx-player://play?server={server}&code={CODE}\n",
            f"plurx-player://play?server=ftp%3A%2F%2Fmedia1&code={CODE}",
            f"plurx-player://play?server=http%3A%2F%2Fmedia1%2Fpath&code={CODE}",
            f"plurx-player://play?server=http%3A%2F%2Fmedia1%2F&code={CODE}",
            f"plurx-player://play?server=http%3A%2F%2Fuser%40media1&code={CODE}",
            f"plurx-player://play?server=http%3A%2F%2Fmedia1%3Fq%3D1&code={CODE}",
            f"plurx-player://play?server=http%3A%2F%2Fmedia1%3A99999&code={CODE}",
            f"plurx-player://play?server=javascript%3Aalert(1)&code={CODE}",
            f"plurx-player://play?server=&code={CODE}",
            "plurx-player://play?" + "x" * 5000,
        ]
        for url in bad:
            with self.subTest(url=url[:80]):
                with self.assertRaises(HandoffUrlError):
                    parse_handoff_url(url)


if __name__ == "__main__":
    unittest.main()
