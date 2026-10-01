"""Focused validator refusals. No served application code is evaluated."""

from pathlib import Path
import subprocess
import unittest

ROOT = Path(__file__).resolve().parents[2]


class EffortWebStaticCase(unittest.TestCase):
    def node(self, body):
        result = subprocess.run(
            ["node", "-e", 'const assert=require("node:assert/strict");'
             'const shape=require("./scripts/web-shape-check");' + body],
            cwd=ROOT, capture_output=True, text=True, timeout=20)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_table_rejects_unknown_kind_and_wrong_include_path(self):
        self.node('''
const wrap=row=>'pub const WEB_ASSETS: &[(&str, WebAsset, &str)] = &[\\n'+row+'\\n];';
const row='    ("app.css", WebAsset::HeadStyle, include_str!("../web/app.css")),';
assert.deepEqual(shape.tableRows(wrap(row)),[{file:"app.css",kind:"HeadStyle"}]);
assert.throws(()=>shape.tableRows(wrap(row.replace("HeadStyle","Other"))));
assert.throws(()=>shape.tableRows(wrap(row.replace("../web/app.css","../web/other.css"))));
assert.throws(()=>shape.tableRows(""));
''')

    def test_generated_block_rejects_stale_duplicate_and_reversed_markers(self):
        self.node('''
shape.exactBlock("prefix<B>actual<E>suffix","<B>","<E>","<B>actual<E>");
for(const text of ["<B>stale<E>","<B>actual<E><B>","<E>actual<B>","actual"])
  assert.throws(()=>shape.exactBlock(text,"<B>","<E>","<B>actual<E>"));
''')

    def test_actual_asset_shape_refuses_order_link_orphan_and_sidecar_drift(self):
        self.node('''
const fs=require("node:fs");
const {shellSource}=require("./tests/web/shell-source.js");
const shell=shellSource(), rust=fs.readFileSync("crates/plurxd/src/http/web.rs","utf8"), doc=fs.readFileSync("docs/clients/WEB-SHELL-LAYOUT.md","utf8");
shape.assetShape(shell,rust,doc,[]);
assert.throws(()=>shape.assetShape(shell,rust,doc,["player/orphan.js"]));
assert.throws(()=>shape.assetShape(shell,rust,doc.replace("../../crates/plurxd/src/web/app.css","wrong.css"),[]));
assert.throws(()=>shape.assetShape({...shell,html:shell.html.replace('/assets/reader.css','/assets/missing.css')},rust,doc,[]));
assert.throws(()=>shape.assetShape({...shell,html:shell.html.replace('/assets/core/theme.js','/assets/TEMP.js').replace('/assets/core/errors.js','/assets/core/theme.js').replace('/assets/TEMP.js','/assets/core/errors.js')},rust,doc,[]));
''')

    def test_dom_shape_refuses_extra_timeline_stop_and_missing_control(self):
        self.node('''
const {shellSource}=require("./tests/web/shell-source.js");
const contract=require("./tests/playback/player-input-contract.json");
const shell=shellSource(); shape.domShape(shell,contract);
assert.throws(()=>shape.domShape({...shell,html:shell.html.replace('id="ptimeline"','id="ptimeline" tabindex="0"')},contract));
assert.throws(()=>shape.domShape({...shell,html:shell.html.replace('id="pbplay"','id="removed"')},contract));
''')

    def test_effort_lane_is_static_and_full_manual_target_is_preserved(self):
        makefile = (ROOT / "Makefile").read_text()
        target = makefile.split("effort-web-static-check:", 1)[1].split(".PHONY:", 1)[0]
        for command in ("scripts/js-check", "node scripts/web-jsconfig --check",
                        "scripts/web-types", "node scripts/web-shape-check", "scripts/contrast-check"):
            self.assertIn(command, target)
        self.assertNotIn(".test.js", target)
        self.assertNotIn("browser-check", target)
        full = makefile.split("web-check: ##", 1)[1].split("## ---- packaging", 1)[0]
        for retained in ("asset-load.test.js", "player-typedef.test.js", "web-hls-startup-browser-check",
                         "subtitle-readiness-browser-check", "player-input-contract.test.js", "player-dom.test.js"):
            self.assertIn(retained, full)
        workflow = (ROOT / ".github/workflows/effort-ci.yml").read_text()
        lane = workflow.split("  web_static:\n", 1)[1].split("  apple_compile:\n", 1)[0]
        self.assertIn("run: make effort-web-static-check", lane)
        self.assertNotIn("playwright", lane.lower())
        self.assertIn("needs: [scope, preflight]", lane)

    def test_info_shape_refuses_unknown_format_empty_modes_and_duplicate_labels(self):
        self.node('''
const original=require("./tests/playback/playback-info-fields.json");
shape.infoShape(original);
for(const change of [f=>f.fields[0].format="unknown",f=>f.fields[0].modes=[],f=>f.fields.push({...f.fields[0],id:"unique"})]) {
  const fixture=structuredClone(original); change(fixture);
  assert.throws(()=>shape.infoShape(fixture));
}
''')
