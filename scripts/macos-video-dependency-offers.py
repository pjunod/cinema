#!/usr/bin/env python3
"""Verify and extract pinned dependency offers without reconstructing Git metadata."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import tarfile

POLICY = Path(__file__).with_name("macos-video-dependency-sources.json")
MODEL = "opus_data-a5177ec6fb7d15058e99e57029746100121f68e4890b1467d4094aa336b6013e.tar.gz"
ALIASES = {"openssl": "openssl-3.6.3", "xz": "v5.8.3", "libpng": "v1.6.58"}


# Exact audited generated outputs of the historical reconstruction. These are
# not a reusable exemption for arbitrary configure/autoreconf modifications.
HISTORICAL_GENERATED = {
    "lame/ACM/ADbg/Makefile.in": {
        "compiled_sha256": "a3ebff443ecb118eb6c7c83bd4447b65f3e3ed3460d2ca74ce05f6179230100e",
        "original_sha256": "6ca61ff92cddad19b968048c99988c966c77a6943dfe25b9cfdbfac6dc6d349a"
    },
    "lame/ACM/Makefile.in": {
        "compiled_sha256": "447275356de270d76a148ba92ef49e235e59c395151afaa87ec7994cbc0b486c",
        "original_sha256": "bec2d6e956aa5add44ec17de9079a5e091a0cd6e9d7768c3a070021975c2a536"
    },
    "lame/ACM/ddk/Makefile.in": {
        "compiled_sha256": "c0cbb11526dfa9298f45e67afcf7d982870c5f9a2ba7d1fd2487890241071ca5",
        "original_sha256": "111df63de7448a1ddd40b300b3d2755a276d254ce8495a1717ef0f74275277b5"
    },
    "lame/ACM/tinyxml/Makefile.in": {
        "compiled_sha256": "feb252042154d590e3a5a8dad529ea4e5913c1055012f810101ce76f92dc3b47",
        "original_sha256": "d33512187403986b5d5aaabe2e5e3057301c24a68708c3d48873d0e1612134a4"
    },
    "lame/Dll/Makefile.in": {
        "compiled_sha256": "4dd8c3c0bcea082d3f0407e0fa19b86347e5ead05e8263fb6ab408d467e52927",
        "original_sha256": "862a57a0dad5d547e7f07100895fc702ff05ae5d88c25f5e29b2c72cd8a6e109"
    },
    "lame/Makefile.in": {
        "compiled_sha256": "78bb6d06ebcd0dc71d869a75e2a4aa305c75afa751ada62e3b415296d624c718",
        "original_sha256": "f4561e3e620946605df15100cd477322f0819858dd93982731fa1e94ef77cb39"
    },
    "lame/aclocal.m4": {
        "compiled_sha256": "58f65159438d2a96c50d554e5879effc67ce4f2525c21c68541cf0b364105480",
        "original_sha256": "bb5375b9d050d63c8d6ee5f9b726a15a3a0d3d08b93c739cc0193c427d258ff7"
    },
    "lame/compile": {
        "compiled_sha256": "ca2177d6b85b76639352270d694bc28b9ba348c315b2427242af886c1d9945f9",
        "original_sha256": "c207b390aac6323062b982214a6c63448e53e6911107993abe96f35fe7a30a18"
    },
    "lame/config.guess": {
        "compiled_sha256": "ac18bbd7dc3769e1646af49ebba331a391829f4a73579b735dc8d439bd1c7f07",
        "original_sha256": "7791fa2c24a0aa966399c7ae9265b1a421b442610db487a429378b7103a6bd28"
    },
    "lame/config.h.in": {
        "compiled_sha256": "98d307748dfd67aa19ab2ca74a316849295a8e507c2d93b875ad6624979fc6a7",
        "original_sha256": "c8887db5e2b18cde3128b928b5bc3c203c7bfa407c436e3f42f98d6969d723fe"
    },
    "lame/config.rpath": {
        "compiled_sha256": "46e05ef0ed1805729438662c040e85b0abdeba5fbedd448c4d79a3f0f3af6250",
        "original_sha256": "9b98b066c0c2902f32984613cb7454b73f1cb93a83422666d73b3c08731a5c80"
    },
    "lame/config.sub": {
        "compiled_sha256": "f9a31e9a3f5b7cbeb8d8c3f2015895a51e7222130114c9c363fcbccd78e4bf6b",
        "original_sha256": "4431bef46ac3d3bee68f283f48d8b94caba57d2f566f8a72b61e92cbad2b8385"
    },
    "lame/configure": {
        "compiled_sha256": "42df2e302306ded631132a7a6500888bba5d924d933d7f88fde01e1aa293e4aa",
        "original_sha256": "c38336c09e42947f55dc2e0cd2605625074e1c69df960656f0b7175ce5471515"
    },
    "lame/depcomp": {
        "compiled_sha256": "e3d3ec05f44de5e3f6100d2894c453a1254a8ad9e2ba3a3aa046476187d9179b",
        "original_sha256": "e44b49f71b265788187993090027193a6cd2b4718f9aa7be34412f537bce6873"
    },
    "lame/doc/Makefile.in": {
        "compiled_sha256": "ccc692ef5f4d35182168a8c9a8642acbbde697376c45319dfa567aae3100af92",
        "original_sha256": "a399bd2262f0263e730e9f539f8ebf8f990a3a8fc8feba458771486c7724df14"
    },
    "lame/doc/html/Makefile.in": {
        "compiled_sha256": "0906ecd4919eec2e522a6d8a22457eea4b902141e71325767c6d9992792a62ed",
        "original_sha256": "150febc18c096328ca6819833703126bb612c7a1112f9587388d19c5a34de288"
    },
    "lame/doc/man/Makefile.in": {
        "compiled_sha256": "e5e9566bf9723c9fd70cd6696ccd885602aa7b13afee5ab48f7ba1f110c214ae",
        "original_sha256": "7ff14d3ebe1ddba4c633798a2e6d866d9f7f6bda93fab28f6ed7a3a689fcc3e2"
    },
    "lame/dshow/Makefile.in": {
        "compiled_sha256": "33a3394affdb3386f6f4215a57b84940531e75227dbcc1c838d936c6abee0a2c",
        "original_sha256": "d334247617f92f8ec9a298d144416fab4b95b51d50aee63e7218aa1c084a003e"
    },
    "lame/frontend/Makefile.in": {
        "compiled_sha256": "a45bc39c755386f88b13b8dc56f583beb28086342f6b39294c78ddcb75057c14",
        "original_sha256": "e07ccc4893d9f52f8c4c0f9ea167520123690781ba3af87a9a3988de18e8e415"
    },
    "lame/include/Makefile.in": {
        "compiled_sha256": "6da9fa29dca38df02873da9b682b7f9c84750d8e2745699d11c00d2bfd53ef2c",
        "original_sha256": "e020777f6f79478b94a48b0b98ec45f661045226a6e0ff9790219c75c91af4ea"
    },
    "lame/install-sh": {
        "compiled_sha256": "776876b3909b096439109a4c7642eb0f9100f6ba3fb42fa93d05351e50a0a7ef",
        "original_sha256": "3d7488bebd0cfc9b5c440c55d5b44f1c6e2e3d3e19894821bae4a27f9307f1d2"
    },
    "lame/libmp3lame/Makefile.in": {
        "compiled_sha256": "f8940c21a4612cd1cdcb92ffcc4cf066eb6923a3ef8d3260a9a64f135f99a9e5",
        "original_sha256": "ee3eae25ef12432eb22ef7ef3aa2e48000f3ac105d4a0f9b66cf8f2fd1e5c7f0"
    },
    "lame/libmp3lame/i386/Makefile.in": {
        "compiled_sha256": "1c9721af506efe92c183ddef06ccefb8d74ca3afb3fb70aa871405c2014d5c73",
        "original_sha256": "eeba835f7e3c78f56aa6ad4cb870bcd4d5ccf6c28bd163e1c1b986ffac0109f7"
    },
    "lame/libmp3lame/vector/Makefile.in": {
        "compiled_sha256": "3c48c7689e5a5cdae983af0f8e9fe9e494b62ca621e7e07bba77a97e96e42f6b",
        "original_sha256": "d3e7c49adfbf46bbcd22a9ab43d55aade6c4289480715551a09028c195780f19"
    },
    "lame/ltmain.sh": {
        "compiled_sha256": "1473fd999be7bb9a36d9d6eeac9fbfe4ed63902fc8c0a64c722559da4a46da95",
        "original_sha256": "30712e3401deb6e6d5255c71f7bd57f374429d220cfc199ba1f2376ab42c2e35"
    },
    "lame/m4/libtool.m4": {
        "compiled_sha256": "3ab7a300db14a3aa7d1986dcec78ec21fbaaea614ca7d82c2f39a73ba0552c4a",
        "original_sha256": "fe3baac94510d4b563ed7562035bdff366e37f9e0ac274bf10e4f22d08e8664e"
    },
    "lame/m4/ltoptions.m4": {
        "compiled_sha256": "2b725d300784a63d5e71aaeac011e11b5b22f6f6017c29967085447632a7e0ae",
        "original_sha256": "4cc29b667909fcde7a08c984367bce1a1902c860acf8774794484a2e1adeb07b"
    },
    "lame/m4/ltsugar.m4": {
        "compiled_sha256": "8a19df00dbbbb911d0e633d88e53c1bdce4b722469d43a105ed2d77e8a7b4dd8",
        "original_sha256": "0896f153a5a40546566028a4272642ae291532f3e65c25fcae950c8812b8c265"
    },
    "lame/m4/ltversion.m4": {
        "compiled_sha256": "0275a2fb0b5f0cf402a9e03bfa99722da9f603ca03d81ba911ae7573558577dc",
        "original_sha256": "40207e691ec7d3f06cedd592e50e44d7bc187b21ad791aeabdd50871b6606799"
    },
    "lame/m4/lt~obsolete.m4": {
        "compiled_sha256": "e35bdbd17dcd0216a2b8a6148ffb5edb941e27f8f90eba8ba7c520043821af4e",
        "original_sha256": "8533006830e1ea9625fc5e4c060e653eedf9d5464a9b2f5f494244ee272e2e2f"
    },
    "lame/mac/Makefile.in": {
        "compiled_sha256": "44eff3f823c1a48aa74cacb81a7280e31943996b50c393e8126830cb01eef136",
        "original_sha256": "e386775bd75cfd3dd4daebcdde46c6fbb9b98726697feeb6757252fdc9ccd654"
    },
    "lame/misc/Makefile.in": {
        "compiled_sha256": "d4d4f1d712e592d315912eb04a083b782bfe0125e92e39212efbbebd77b4e4ff",
        "original_sha256": "ba50528fe9661cc79e4165819c9feb5fa48b04a1664f01214f4c82077ab657ba"
    },
    "lame/missing": {
        "compiled_sha256": "fb41d901ad637538e2a5fbaba061bf9cba408d136e35daef8552f1d1f022ec5c",
        "original_sha256": "a9865db4f39574ff128c0312c367f070d20f81847817021ecce95fd70a610c9d"
    },
    "lame/mpglib/Makefile.in": {
        "compiled_sha256": "f02edfaeb94cf386da0cca7f918a9aa7d195732c46b1c6ee34388858f2adc1c5",
        "original_sha256": "1c74f5fc2e5d8b4914e60a51597dcd894f49e6df89dc222690a5acf72f027a08"
    },
    "lame/vc_solution/Makefile.in": {
        "compiled_sha256": "108dca046d59bf4698ddd0072bfcd42cfdfa7c3c7850afd37be53a32c6513ff4",
        "original_sha256": "0b1cd2c99bcf8e2ba5e8ab5c9949a0f2f7101edcee8d2b0322bbc881d73baee6"
    },
    "zlib/Makefile": {
        "compiled_sha256": "1306b8dd6a83c94c78ae45aa022ffe3e88878d75cb2a4e110a5a83e3a5616a4b",
        "original_sha256": "ef23b08ce01239843f1ded3f373bfc432627a477d62f945cbf63b2ac03db118a"
    },
    "zlib/zconf.h": {
        "compiled_sha256": "0718a11beb3295b345fb29a63b44b654b282d82c4cd0513f6225587f2b29b8bb",
        "original_sha256": "cb7c2c84211473b4699223edd363d3207b43b9578e739b5bf638f42204ea6e0f"
    }
}


def digest(path):
    value = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def archive_name(role, facts):
    if facts["kind"] in ("git-source-archive", "git-submodule-source-archive"):
        return role + "-" + facts["commit"] + ".tar.gz"
    if facts["kind"] == "svn-source-archive":
        return role + "-svn-source.tar.gz"
    if facts["kind"] == "upstream-source-archive":
        return role
    raise ValueError("unknown dependency source kind")


def verify_offers(directory, roles=None):
    expected = json.loads(POLICY.read_text())
    manifest_path = directory / "manifest.json"
    if manifest_path.is_symlink() or not manifest_path.is_file():
        raise ValueError("dependency manifest must be a regular file")
    actual = json.loads(manifest_path.read_text())
    if not actual or set(actual) - set(expected):
        raise ValueError("unknown dependency source role")
    required = set(expected) if roles is None else set(roles)
    if not required <= set(actual):
        raise ValueError("missing required dependency source role")
    for role in actual:
        if actual[role] != expected[role]:
            raise ValueError("dependency source identity mismatch: " + role)
        if roles is not None and role not in required:
            continue
        path = directory / archive_name(role, actual[role])
        if path.is_symlink() or not path.is_file() or digest(path) != actual[role]["sha256"]:
            raise ValueError("dependency source archive mismatch: " + role)
    return actual


def stage_offers(directory, destination):
    actual = verify_offers(directory)
    destination.mkdir()
    for role, facts in actual.items():
        name = archive_name(role, facts)
        target = destination / name
        shutil.copyfile(directory / name, target)
        if digest(target) != facts["sha256"]:
            raise ValueError("dependency archive changed while staging")
        target.chmod(0o444)
    opus = directory / archive_name("opus", actual["opus"])
    with tarfile.open(opus) as archive:
        notice = archive.extractfile("COPYING").read()
        recipe = archive.extractfile("autogen.sh").read()
    if hashlib.sha256(notice).hexdigest() != "01e1167d54a096d123cf6dfbbeb19587278845c6481d2d66d545669846079551":
        raise ValueError("pinned Opus license notice changed")
    if ('dnn/download_model.sh "' + actual[MODEL]["sha256"] + '"').encode() not in recipe:
        raise ValueError("Opus model is not bound to the pinned official recipe")
    (destination / "opus-COPYING").write_bytes(notice)
    (destination / "opus-model-license-scope.json").write_text(json.dumps({
        "model_archive": MODEL, "model_sha256": actual[MODEL]["sha256"],
        "source_role": "opus", "recipe": "autogen.sh", "recipe_sha256": hashlib.sha256(recipe).hexdigest(),
        "notice": "opus-COPYING", "notice_sha256": hashlib.sha256(notice).hexdigest(),
        "official_implementation_license": "https://www.opus-codec.org/license/",
        "scope": "Pinned Opus implementation COPYING retained; model archive contains no separate license notice."}, indent=2, sort_keys=True) + "\n")
    (destination / "manifest.json").write_text(json.dumps(actual, indent=2, sort_keys=True) + "\n")
    return {"manifest_sha256": digest(destination / "manifest.json"), "roles": sorted(actual)}


def verify_preparation(root):
    facts = json.loads((root / "prepared-source.json").read_text())["dependency_source_inputs"]
    paths = {"manifest_sha256": root / "dependency-source-inputs/manifest.json",
             "recipe_bindings_sha256": root / "dependency-recipe-bindings.json",
             "helper_sha256": Path(__file__), "policy_sha256": POLICY}
    for key, path in paths.items():
        if path.is_symlink() or not path.is_file() or digest(path) != facts.get(key):
            raise ValueError("prepared dependency input changed: " + key)
    return json.loads(paths["recipe_bindings_sha256"].read_text())


def verify_recipe(root, recipe, role):
    bindings = verify_preparation(root)
    if recipe not in bindings or role not in bindings[recipe]["roles"]:
        raise ValueError("dependency role is not bound to this pinned recipe")
    path = root / "source" / recipe
    if path.is_symlink() or digest(path) != bindings[recipe]["sha256"]:
        raise ValueError("dependency recipe changed after preparation")
    return {"recipe": recipe, "recipe_sha256": digest(path)}


def copy_upstream(root, role, recipe):
    binding = verify_recipe(root, recipe, role)
    directory = root / "dependency-source-inputs"
    actual = verify_offers(directory, [role])
    if actual[role]["kind"] != "upstream-source-archive":
        raise ValueError("requested source is not an upstream archive")
    source = directory / archive_name(role, actual[role])
    destination = Path.cwd() / source.name
    shutil.copyfile(source, destination)
    if digest(destination) != actual[role]["sha256"]:
        raise ValueError("upstream source changed while copying")
    record = {**binding, "role": role, "recipe_revision": actual[role]["sha256"],
              "inputs": [{"role": role, "archive": source.name, "sha256": actual[role]["sha256"]}],
              "extraction_helper_sha256": digest(Path(__file__))}
    with (root / "dependency-source-consumption.jsonl").open("a") as output:
        output.write(json.dumps(record, sort_keys=True) + "\n")
    return record


def extract_source(root, role, revision, destination, recipe):
    binding = verify_recipe(root, recipe, role)
    directory = root / "dependency-source-inputs"
    expected = json.loads(POLICY.read_text())
    if role not in expected:
        raise ValueError("unknown requested dependency role")
    entry = expected[role]
    if str(entry.get("commit", entry.get("revision", entry["sha256"]))) != revision and ALIASES.get(role) != revision:
        raise ValueError("dependency recipe revision mismatch")
    selected = [role] + sorted(name for name in expected if name.startswith(role + "--"))
    if role == "opus":
        selected.append(MODEL)
    actual = verify_offers(directory, selected)
    if destination.exists() or destination.is_symlink():
        raise ValueError("dependency extraction destination already exists")
    destination.mkdir()
    records = []
    for name in selected:
        path = directory / archive_name(name, actual[name])
        if name == MODEL:
            shutil.copyfile(path, destination / name)
        else:
            target = destination
            if name != role:
                target = destination.joinpath(*name[len(role) + 2:].split("--"))
                target.mkdir(parents=True, exist_ok=True)
            with tarfile.open(path) as archive:
                archive.extractall(target, filter="data")
        records.append({"role": name, "archive": path.name, "sha256": actual[name]["sha256"]})
    record = {**binding, "role": role, "recipe_revision": revision, "inputs": records,
              "extraction_helper_sha256": digest(Path(__file__))}
    with (root / "dependency-source-consumption.jsonl").open("a") as output:
        output.write(json.dumps(record, sort_keys=True) + "\n")
    return record


def verify_consumption(root):
    verify_preparation(root)
    directory = root / "dependency-source-inputs"
    actual = verify_offers(directory)
    receipt = root / "dependency-source-consumption.jsonl"
    if receipt.is_symlink() or not receipt.is_file():
        raise ValueError("missing actual dependency consumption receipt")
    rows = [json.loads(line) for line in receipt.read_text().splitlines() if line]
    if not rows:
        raise ValueError("empty dependency consumption receipt")
    consumed = set()
    for row in rows:
        role = row["role"]
        binding = verify_recipe(root, row["recipe"], role)
        if row.get("recipe_sha256") != binding["recipe_sha256"] or row.get("extraction_helper_sha256") != digest(Path(__file__)):
            raise ValueError("consumed recipe/helper identity mismatch")
        if role not in actual:
            raise ValueError("unknown consumed dependency role")
        expected_revision = str(actual[role].get("commit", actual[role].get("revision", actual[role]["sha256"])))
        if row["recipe_revision"] != expected_revision and ALIASES.get(role) != row["recipe_revision"]:
            raise ValueError("consumed recipe revision mismatch")
        required = {role} | {name for name in actual if name.startswith(role + "--")}
        if role == "opus":
            required.add(MODEL)
        if {item["role"] for item in row["inputs"]} != required:
            raise ValueError("consumed dependency closure mismatch")
        for item in row["inputs"]:
            if item["archive"] != archive_name(item["role"], actual[item["role"]]) or item["sha256"] != actual[item["role"]]["sha256"]:
                raise ValueError("consumed archive identity mismatch")
            consumed.add(item["role"])
    required = set(actual)
    config = root / "source/config.h"
    if config.is_file() and "#define ARCH_X86_64 1" in config.read_text():
        required.discard("Ne10")
    if consumed != required:
        raise ValueError("actual consumed closure does not cover pinned build inputs")
    return {name: actual[name] for name in sorted(consumed)}


def audit_historical_sources(root, offers, logs):
    """Compare original source members; refuse unexplained historical edits."""
    actual = verify_offers(offers)
    changes = []
    generated_lame = {"aclocal.m4", "compile", "config.guess", "config.h.in", "config.rpath",
                      "config.sub", "configure", "depcomp", "install-sh", "ltmain.sh", "missing",
                      "m4/libtool.m4", "m4/ltoptions.m4", "m4/ltsugar.m4", "m4/ltversion.m4", "m4/lt~obsolete.m4"}
    for role, facts in actual.items():
        if facts["kind"] == "upstream-source-archive":
            continue
        target = root / "source/builder/build" / Path(*role.split("--"))
        with tarfile.open(offers / archive_name(role, facts)) as archive:
            for member in archive:
                if not member.isfile():
                    continue
                relative = Path(member.name)
                if relative.is_absolute() or ".." in relative.parts:
                    raise ValueError("unsafe original source member")
                path = target / relative
                if path.is_symlink() or not path.is_file():
                    raise ValueError("historical original source missing: " + role + "/" + member.name)
                original = archive.extractfile(member).read()
                current = path.read_bytes()
                if current == original:
                    continue
                transform = None
                if role == "libbluray" and member.name.startswith("src/libbluray/disc/") and member.name.endswith((".c", ".h")):
                    if current == original.replace(b"dec_init", b"libbluray_dec_init"):
                        transform = "pinned-libbluray-symbol-substitution"
                if role == "libbluray" and member.name == "src/meson.build" and current == original.replace(b"-DBLURAY_API_EXPORT", b"-DBLURAY_API_EXPORT_DISABLED"):
                    transform = "pinned-libbluray-export-substitution"
                if role == "libudfread" and member.name == "src/meson.build" and current == original.replace(b"-DUDFREAD_API_EXPORT", b"-DUDFREAD_API_EXPORT_DISABLED"):
                    transform = "pinned-libudfread-export-substitution"
                if role == "openssl" and member.name == "Configure":
                    expected = b"".join(line.rstrip(b"\n") + b'"apps",\n' if line.startswith(b"my @disablables =") else line for line in original.splitlines(keepends=True))
                    if current == expected:
                        transform = "pinned-openssl-disable-apps-substitution"
                if role == "lame" and (member.name in generated_lame or member.name.endswith("Makefile.in")) and "autoreconf -fi" in logs:
                    transform = "executed-lame-autoreconf-generated-output"
                if role == "zlib" and member.name in {"Makefile", "zconf.h"} and "./configure" in logs:
                    transform = "executed-zlib-configure-generated-output"
                if transform in {"executed-lame-autoreconf-generated-output", "executed-zlib-configure-generated-output"}:
                    expected = HISTORICAL_GENERATED.get(role + "/" + member.name)
                    observed = {"original_sha256": hashlib.sha256(original).hexdigest(), "compiled_sha256": digest(path)}
                    if observed != expected:
                        raise ValueError("historical generated output is not the exact audited transformation: " + role + "/" + member.name)
                if transform is None:
                    raise ValueError("unexplained historical source difference: " + role + "/" + member.name)
                changes.append({"role": role, "path": member.name, "transform": transform,
                                "original_sha256": hashlib.sha256(original).hexdigest(), "compiled_sha256": digest(path)})
    return changes


def retain_historical_build(root, evidence, lineage_path):
    """Truthful historical closure, never a future-helper consumption receipt."""
    lineage = json.loads(lineage_path.read_text())
    if lineage.get("mode") != "historical-archive-build":
        raise ValueError("historical lineage must identify its mode explicitly")
    offers = Path(lineage["offers"])
    verify_offers(offers)
    retained = evidence / "historical-build-lineage"
    retained.mkdir()
    log_text = ""
    files = lineage["executed_files"]
    if not files or not any(row["kind"] == "build-log" for row in files) or not any(row["kind"] == "extraction-helper" for row in files):
        raise ValueError("historical build requires actual logs and executed extraction helper")
    manifest = []
    for index, row in enumerate(files):
        path = Path(row["path"])
        if row["kind"] not in {"build-log", "extraction-helper", "transformation-helper", "tool-provenance"}:
            raise ValueError("unknown historical evidence kind")
        if path.is_symlink() or not path.is_file() or digest(path) != row["sha256"]:
            raise ValueError("historical executed evidence changed")
        name = str(index) + "-" + path.name
        shutil.copyfile(path, retained / name)
        manifest.append({"name": name, "kind": row["kind"], "sha256": row["sha256"]})
        if row["kind"] == "build-log":
            log_text += path.read_text(errors="replace")
    if "archive-source" not in log_text:
        raise ValueError("historical logs do not show archive reconstruction")
    differences = audit_historical_sources(root, offers, log_text)
    (retained / "manifest.json").write_text(json.dumps({"mode": lineage["mode"], "files": manifest, "original_source_differences": differences}, indent=2, sort_keys=True) + "\n")
    stage_offers(offers, evidence / "dependency-source")
    closure = retain_compiled_snapshot(root, evidence, Path(lineage["tool_source"]) if lineage.get("tool_source") else None)
    return {"mode": "historical-archive-build", "dependency_sources": json.loads(POLICY.read_text()),
            "historical_lineage_sha256": digest(retained / "manifest.json"), **closure}


def retain_archive_build(root, evidence):
    """Retain verified future-recipe inputs and the actual compiled source closure."""
    consumed = verify_consumption(root)
    offers = evidence / "dependency-source"
    stage_offers(root / "dependency-source-inputs", offers)
    recipes = evidence / "compiled-build-recipes"
    for name in ("dependency-recipe-bindings.json", "dependency-source-consumption.jsonl"):
        shutil.copyfile(root / name, recipes / name)
    shutil.copytree(root / "extraction-helpers", recipes / "extraction-helpers")
    return {"mode": "verified-archive-recipe-consumption", "dependency_sources": consumed,
            **retain_compiled_snapshot(root, evidence)}


def retain_compiled_snapshot(root, evidence, tool_source=None):
    tool_files = {}
    if tool_source is not None:
        pinned = {"automake": "23c091faee8dac047b0670d8c10da030bf104610fc064b904407de1a1ff8f437",
                  "libtool": "719ae9c4597d0198ee77da6bb09dfcb6f68ae0302330f1a1e61503648e59fdd2"}
        retained_tools = evidence / "source-generation-tools"
        retained_tools.mkdir()
        for name, expected in pinned.items():
            original = tool_source / "archives" / (name + ".tar.gz")
            if original.is_symlink() or digest(original) != expected:
                raise ValueError("source-generation tool archive mismatch")
            shutil.copyfile(original, retained_tools / original.name)
            with tarfile.open(original) as archive:
                for member in archive:
                    if member.isfile():
                        tool_files[str((tool_source / member.name).resolve())] = {"sha256": hashlib.sha256(archive.extractfile(member).read()).hexdigest(), "archive": original.name, "archive_sha256": expected, "member": member.name}

    snapshot = evidence / "compiled-source.tar.gz"
    index = {}
    # Keep generated C/headers, configuration, licenses and resources as well as
    # original sources. Omit native objects/binaries and repository metadata.
    with tarfile.open(snapshot, "w:gz") as archive:
        for path in sorted((root / "source").rglob("*")):
            relative = path.relative_to(root / "source")
            if any(part in {".git", ".svn"} for part in relative.parts):
                continue
            if path.suffix in {".o", ".a", ".dylib", ".so", ".d", ".pyc"}:
                continue
            if str(relative) in {"ffmpeg", "ffprobe", "builder/ffmpeg", "builder/ffprobe"}:
                continue
            if path.is_symlink():
                resolved = path.resolve()
                if not resolved.is_relative_to((root / "source").resolve()):
                    if str(resolved) not in tool_files or not resolved.is_file() or digest(resolved) != tool_files[str(resolved)]["sha256"]:
                        raise ValueError("compiled source symlink escapes verified source-generation tools")
                    # Retain actual tool-provided source as a regular member;
                    # no task-local absolute symlink survives redistribution.
                    info = archive.gettarinfo(str(resolved), arcname=str(relative))
                    with resolved.open("rb") as content:
                        archive.addfile(info, content)
                    index[str(relative)] = {**tool_files[str(resolved)], "source": "verified-source-generation-tool", "original_symlink_target": tool_files[str(resolved)]["member"]}
                else:
                    archive.add(path, arcname=str(relative), recursive=False)
                    index[str(relative)] = {"symlink": str(path.readlink())}
            elif path.is_file():
                index[str(relative)] = {"sha256": digest(path)}
                archive.add(path, arcname=str(relative), recursive=False)
                if digest(path) != index[str(relative)]["sha256"]:
                    raise ValueError("compiled source changed while retaining snapshot")
    (evidence / "compiled-source-index.json").write_text(json.dumps(index, indent=2, sort_keys=True) + "\n")
    return {"compiled_source_sha256": digest(snapshot),
            "compiled_source_index_sha256": digest(evidence / "compiled-source-index.json")}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--upstream", action="store_true")
    parser.add_argument("--recipe", required=True)
    parser.add_argument("role", nargs="?")
    parser.add_argument("revision", nargs="?")
    parser.add_argument("destination", type=Path, nargs="?")
    args = parser.parse_args()
    if args.upstream:
        if not args.role or args.revision or args.destination:
            parser.error("upstream copy does not take extraction arguments")
        record = copy_upstream(args.root, args.role, args.recipe)
    else:
        if not all((args.role, args.revision, args.destination)):
            parser.error("source extraction requires role, revision and destination")
        record = extract_source(args.root, args.role, args.revision, args.destination, args.recipe)
    print(json.dumps(record, sort_keys=True))


if __name__ == "__main__":
    main()
