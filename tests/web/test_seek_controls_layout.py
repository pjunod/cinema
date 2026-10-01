"""Render the shipped transport at constrained widths (requires Playwright)."""
from pathlib import Path
import unittest

from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parents[2]
WEB = ROOT / "crates/plurxd/src/web"


class SeekControlsLayoutTests(unittest.TestCase):
    def test_seek_controls_fit_landscape_and_embedded_players(self):
        markup = next(line for line in (WEB / "index.html").read_text().splitlines()
                      if 'id="modal"' in line)
        css = (WEB / "app.css").read_text()
        with sync_playwright() as playwright:
            browser = playwright.chromium.launch()
            try:
                page = browser.new_page(is_mobile=True, has_touch=True)
                for viewport, player_width, inline in [
                    (320, 320, True), (390, 390, True),
                    (320, 320, False), (390, 390, False),
                    (667, 667, False), (736, 736, False),
                    (1280, 600, False),
                ]:
                    with self.subTest(viewport=viewport, player=player_width, inline=inline):
                        page.set_viewport_size({"width": viewport, "height": 780})
                        page.set_content(
                            '<meta name="viewport" content="width=device-width,initial-scale=1">'
                            + "<style>" + css + "</style>" + markup
                        )
                        page.evaluate("""([viewport, width, inline]) => {
                          const modal = document.getElementById('modal');
                          modal.className = 'modal open watch-host' +
                            (width === viewport && !inline ? ' watch-full' : '');
                          if (inline || width !== viewport)
                            modal.style.cssText = `top:20px;left:0;width:${width}px;height:380px`;
                          if (inline) document.getElementById('player').classList.add('watch-inline');
                          if (viewport <= 550) document.getElementById('pblarger').remove();
                          if (inline) document.getElementById('pbinfo').remove();
                          for (const id of ['pbaudio', 'pbsubs', 'pbpip'])
                            document.getElementById(id).style.display = '';
                          for (const id of ['ploading', 'psurface', 'pindicator', 'statsov'])
                            document.getElementById(id).style.display = 'none';
                        }""", [viewport, player_width, inline])
                        buttons = page.locator("#ptransport button")
                        boxes = buttons.evaluate_all("""els => els.map(e => {
                          const r = e.getBoundingClientRect();
                          return {id:e.id, x:r.x, y:r.y, w:r.width, h:r.height};
                        })""")
                        self.assertTrue({"pbback30", "pbback", "pbplay", "pbforward", "pbforward30"}
                                        .issubset({box["id"] for box in boxes}))
                        timeline = page.locator("#ptimeline").bounding_box()
                        self.assertLessEqual(timeline["y"] + timeline["height"],
                                             min(box["y"] for box in boxes) + 1)
                        for i, box in enumerate(boxes):
                            self.assertGreaterEqual(box["x"], 0, box)
                            self.assertLessEqual(box["x"] + box["w"], player_width + 1, box)
                            self.assertGreaterEqual(box["w"], 44, box)
                            self.assertGreaterEqual(box["h"], 44, box)
                            for other in boxes[i + 1:]:
                                overlap_x = min(box["x"] + box["w"], other["x"] + other["w"]) - max(box["x"], other["x"])
                                overlap_y = min(box["y"] + box["h"], other["y"] + other["h"]) - max(box["y"], other["y"])
                                self.assertFalse(overlap_x > 1 and overlap_y > 1, (box, other))
                            page.locator("#" + box["id"]).click(trial=True)
            finally:
                browser.close()


if __name__ == "__main__":
    unittest.main()
