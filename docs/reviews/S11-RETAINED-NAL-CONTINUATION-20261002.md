# Retained NAL continuation — sixteen header contexts, not original S11 closure

**Status:** done — scoped retained-header ledger only · **Written:** 2026-10-02

Original S11 qualification remains open. This evidence-only continuation
does not claim independent review, gate approval or integration by itself.

Companion to the [original Grain720 evidence ledger](S11-GRAIN720-NAL-EVIDENCE-20261002.md),
[internal acquisition ledger](../streaming/S11-INTERNAL-ROLLING-CELLS-2026-10-02.md)
and [codec qualification plan](../streaming/CODEC-AND-GPU-QUALIFICATION.md).
This adds Grain360/480 header-receipt identities and thirteen new Linux
retained-header results, preserving earlier route/local/pressure refusals.
It changes no product, parser, GOP policy or acceptance threshold. Original
#737 ledger/snapshots remain unchanged; its fifteen remaining contexts
statement describes that earlier one-context boundary, not current coverage.

## Header coverage — sixteen of sixteen; original S11 still open

| Context | SPS geometry | Result bytes | Raw-private result SHA-256 |
|---|---|---:|---|
| Grain720 (#737) | 1280×720 | 343739 | `9a286a6cbd781fa3bcf4f5a1180522f23f9321eb44afdb9c46f06b4db0ec995a` |
| Grain360 | 640×360 | 340905 | `57d2760674fea9296215245ae0d8ff84282d2e49cfaa39e4d8b9903022df64b5` |
| Grain480 | 854×480 | 341442 | `61522471bed4ee7f87072658a5226ecf8ed2994eb084902141ec0260901c43ce` |
| Grain1080 | 1920×1080 | 348316 | `1ab4338c5610712cc8d035af5176b9326ebbdb1d1d9663828c086bea5eb35902` |
| H264360 | 640×360 | 341403 | `52b95711cbb7867e7342f480c9b2dde0f447c6b7880fc6a8acd8f5aa8aabe757` |
| H264480 | 854×480 | 342119 | `be9a74895e78f533b2960e1639e300c3f97d9658551ddfb31a427a17b9d31a25` |
| H264720 | 1280×720 | 344326 | `a22aaab8a67b1f9027ea4d54f83294f6ed29c394ba8df4bb248de29aeab3f10b` |
| H2641080 | 1920×1080 | 348595 | `f02dbab0e9a58c6bbb9fdc05ec4b19064b3b68c5440c4947141253ad01a2862b` |
| SDR4K360 | 640×360 | 341430 | `49274a979818b30febb7a17fc4a7c4927f534c7f910ee7a4b8e8badb50de9051` |
| SDR4K480 | 854×480 | 342132 | `7520a79a123b5bfb172f8f1f12e8b3340523c585c0d5c4937132caeba052ec5c` |
| SDR4K720 | 1280×720 | 344456 | `0960cfdc5f8ffaf5332c5c7c06c8a1e1bb0fb285f2459ab95158ba1df6f6e73c` |
| SDR4K1080 | 1920×1080 | 348786 | `66ca5ab7a2f624c3a545aee8b1815793f9f7eecddd6f3c4f7d58b392f364110e` |
| PQ4K360 | 640×360 | 341600 | `64f7df340a78a16f06da0cf0e6ba6856f91f7cf1229e6a65467ffeb2a9170bd5` |
| PQ4K480 | 854×480 | 342370 | `ef4d0bd7a522987f4c5d3944cc717e9b40196f7231040446e9a0f548d2b6e603` |
| PQ4K720 | 1280×720 | 344625 | `81e4dbaada252b396ea8d4ba6782084e94ce1ccc5785720d01624db810f587b0` |
| PQ4K1080 | 1920×1080 | 348857 | `2b27628fd23c3613eda37840e072952aa5995b296f95f808bddb73127258c1c5` |

**How to read it:** each context has 35 contiguous complete retained TS
objects and 1680 AUD/picture/PTS-matched access units, with 35 first IDRs.
Sorted presentation timestamps are unique, with exact3750/90000-second
spacing:24fps/70seconds including final frame duration. Decode-order B-frame
PTS reordering is not presentation drift. Referenced SPS/PPS, slice identity,
transport continuity and actual type-5 headers are parsed, not inferred from
old packet key flags, one-PES-equals-frame assumptions or arbitrary byte scans.
Old raw probe files are comparison inputs; no probes ran again.

Grain720's TWO extra IDRs remain at13.958333333… and38seconds:37total.
All other fifteen contexts have35total, no extras. Every old key-flag timestamp
matches a parsed IDR within the unchanged one-90kHz-tick tolerance. All thirteen
Linux results have maximum0.030000tick difference from old decimal rounding;
post-hoc exact equality would be overstrict, not parser failure or another run.

Nine original tiny parser controls remain once-successful and unreplayed.
Each credited context has one successful census; operational refusals and
failed sandbox attempt retain separate outcomes. No entropy/pixel decode,
closed-GOP reference independence or decoder/device random-access acceptance
follows. Counts remain **16 internal acquisitions /4 synthetic inputs /
0 original fully-qualified public cells**.

## Original producer and new Linux oracle have different identities

Measured producer source is `fb4360792a3ae077aa73a40ee30cfcdd024ff445`, tree
`cc94375018a2fb569833d783dbdc9da93336fdf0`, ARM64 binary SHA-256
`656e75891579d9526de544874f92651649ddf8f85071413b0bb207b4aabc524a` and image
`b7bc6f794e9d6aebae8fd54c32c513c3507173729e40c1b5048097e09e23d303`.
Synthetic source identities are bound by the existing acquisition ledger and
original retained receipts, not relabeled current effort or real-film inputs.
PQ-labelled context names refer to the synthetic source, not output HDR or
calibrated tone-map fidelity: all delivered objects parsed here are H264.

Grain360/480, like Grain720, used bounded offline Docker/Python3.11 workers.
Their receipts bind2CPU/1GiB/equal total swap/PID64/network-none/read-only,
60sparser/90souter/512MiBinput/1MiBresults, independently absent exact
containers/volumes/staging/watchdogs before Docker return. Actual cgroup
peaks29782016B/30146560B, no OOM/max events. Private canonical receipt SHAs:
`e4eac9a91b764a8d8132fe5332017ed5cda8e4efa4a952a15841d696357c929c` and
`ea48f04777c3a7ef0cc241447b1da829039e41eb17f2bef40256d340650ae547`.
These successful contexts were retained, not replayed.

Grain1080 plus H264/SDR/PQ twelve used separately admitted Linux x86_64
Python on fresh private UID1000 nonce leaves, not that ARM64 container.
They reused the exact [parser snapshot](evidence/s11-grain720-nal-20261002/parser.py),
SHA-256 `56825d98839fa31757112bbc7ebeba731e8266cf3bc8ea93ec15cb45af689c80`.
Private immutable source identities:

| Snapshot | SHA-256 |
|---|---|
| Linux routes1 runner | `4612a0518a145ad1c139f008ed80b66c201a7b015717ef2f45062bbd4de09cd4` |
| Independent outer watcher | `57e018f56bc5526c3c7bde0bf4b938611a1bef7b6efca9699d02e1bc8c8c64df` |
| Host-only authenticated transport | `1c64256829756585f2c50f86f27aa1dd79ec69ecba32829371432e96b79ebdb4` |
| Fixed remaining-context map | `d84a82e3456410dcd09af8e064dcf21fe8b7874b78ea36ab00913e4e7fb56f40` |

Host-only authentication was never a transfer input. Fresh namespace/only
loopback/exact inert-route allowlist and syscall network/escape denial were
required. Actual worker stdout records affinity[0,1], hard CPU[60,60]s,
address-space[1073741824,1073741824]B and file[900000,900000]B.
These are Linux **per-process** limits, not a Docker cgroup, aggregate1GiB
limit or LinuxPID64 assertion. Host total240s/independent outer90s+15s
cleanup margin/input≤512MiB/total persisted results≤1MiB are per context.
Contexts were serialized; no pooled input/result budget. Shared/unreserved
load was telemetry, not quiet-host/throughput evidence. MemAvailable≥4GiB,
disk≥1GiB and PSIavg10 maxima CPU25/memory1/I/O5 remained unchanged.

## Thirteen Linux passes — actual result and cleanup identities

Root ran each successful cell once. New local read-only audit verified the
exported JSON/control/manifest pins and geometry/AU/grid facts; it did not
read original media/probe bytes or invoke parser/SSH/controls/tests.
Every72-file manifest was bound before transfer, using original held
identities and a separately sanitized acquisition receipt. All7exports,
worker/outer source+entry+manifest/result identities and actual hard limits
matched. All6SSHphases exited0/reaped. Successful captured /var/tmp + /proc
inventories proved each exact nonce leaf and its four named PIDs absent
following export acknowledgement; worker/supervisor groups also absent.
A failed stat or missing reply is never treated as absence.

| Context |72-file copied bytes | Whole/worker/outer seconds | Child maxRSS KiB | Local bundle bytes |
|---|---:|---|---:|---:|
| Grain1080 | 74658767 | 25.225919/18.245040/18.364231 | 35580 | 383437 |
| H264360 | 13351827 | 5.198817/2.254533/2.326566 | 29400 | 376444 |
| H264480 | 20508235 | 7.324357/3.930567/4.049733 | 29560 | 377159 |
| H264720 | 38418501 | 11.873214/7.326486/7.432864 | 31212 | 379445 |
| H2641080 | 74180498 | 17.074726/11.172478/11.269941 | 35172 | 383709 |
| SDR4K360 | 13339077 | 5.373700/2.194509/2.326482 | 28692 | 376481 |
| SDR4K480 | 20504510 | 6.028958/2.236844/2.310574 | 29544 | 377187 |
| SDR4K720 | 38454067 | 11.731686/7.257914/7.350861 | 31508 | 379563 |
| SDR4K1080 | 74214184 | 21.697119/15.896085/16.013110 | 38412 | 383914 |
| PQ4K360 | 13366914 | 6.884630/3.312436/3.421065 | 28924 | 376661 |
| PQ4K480 | 20528399 | 8.499710/5.218547/5.332212 | 29556 | 377424 |
| PQ4K720 | 38408774 | 10.639972/6.591433/6.675113 | 31352 | 379737 |
| PQ4K1080 | 74291467 | 17.772316/11.815281/11.908056 | 35552 | 383991 |

MaxRSS is child telemetry, not aggregate/cgroup peak. Complete private batch
receipt SHA-256 `176e29409aa3a7fae3f03a25323d84b493d0cd96df98f91839761c27f818f070`
binds all retained roots, all ten files per bundle, original/sanitized receipt
pins, exact nonces/leaf identities/PID sets and successful inventory hashes.
Local audit JSON SHA-256
`036de7d79d1863c3c16cb27afa6edffd056e1e8f08b4eb310ebcecd58d28391c`.
The portable tables below identify raw-private bytes; they are not raw-data
publication or a claim these private bundles are accessible from Git.

| Context | Host final SHA-256 | Held manifest SHA-256 | Raw control SHA-256 |
|---|---|---|---|
| Grain1080 | `4323393150e0537fc95e29d9ab945dac68c0308106f7d9426eef045c4a55e7a1` | `8b0aa548fa1aaf40daab00e72ca20793c2b4c2842d1e2582564e16da191a75c5` | `badfe035b641b15b9270d244a733083b9e4069824fe07f67343c5279b7659651` |
| H264360 | `8c38717acee97586d9593813def10d4b7d869640d8134e095c32c2f8ff56d855` | `1080e5a3549ae344e56eba0369f304f7c05d4de6b2b1ac219e6f165b3f72e2ea` | `185e7c048d00b3286d63e8346f1a3399a52af035ce8e75cf789913cd0e15f8aa` |
| H264480 | `3d53d4e976392b2074435a2c1ebb20b3935d99c001253fc089b70e8aede7d6f5` | `e18ee638d6ba4e5e7b906fd2b4f77866565444aa93d027df451da5dbf2624095` | `045be41f3d391d71bf0ab5fc35b084ef0a61898ba5e30daccad7d94725b460df` |
| H264720 | `5b2461f9b357173aff228a6269b88973bdb44b62ce17686a4f20d29e4b382e30` | `c84f4ffa7e4d8fc990642a7b5b9868c7b312d1e2fdcda7e5d88e461f5764da03` | `5c92242f519b19b5ecad1469bb1b6c2b125db10469f611724da4fa01d28b3bc8` |
| H2641080 | `fdd349f94c1735578b1ddadf5b3939948c097c8369e746c7670b2a5acbaa1a4e` | `b70f51414ade0b412ad04013e1beb81087a6b2600fe7c873c764667f8fc48299` | `d0d9e5769f1a4c257e5293c5e818ae13aeb7fabd2466a8404ca202b56597f751` |
| SDR4K360 | `17aa7312215a0b6c666bcaaced8a05e63cd48a526dc7f3aa68d9af7e892a79ae` | `e8300a4002db6b1078539bf0cdc5205734673797a9c2e6ece869eafa27703156` | `48584299b1fb22df592faa27b08f31de627e3bbba0f212a8f04610078f4ca2d4` |
| SDR4K480 | `2a848b56308b96a1d0273b0165cda2ce17ac7f2c45a1d046f88986595bdc477e` | `4acea02b721f96e1da4c16a1acfa642f829d821c11f456d46fe381de11e7934b` | `2371ff577f9167af7b34922c75721ded7c45f44514d286aec33547ed4b6b4277` |
| SDR4K720 | `2d5ad88aaea9a46af5b542852c385c607335b9eb42228ce32fb2046e633d27ad` | `62793cde677c82dd4c503409fb4dc25d4fe0fb18c7db0fb250252a1ced487929` | `da4d23e43c8c65958c0af4d339b88423fe44bd67229ac73f0ef2116bd00e804c` |
| SDR4K1080 | `ebfc99bf117ac7d160655d8d73a26089b12a199d4827718206e1871f1b347482` | `b32d7fe7d1a6677e44839965673acd4acf6a4b66e4bef8c4a7b99e6c136522e5` | `e7b167c70adc57cb080d84afacf7ccba9dcc15b944b8a76cc2faccc322de7b81` |
| PQ4K360 | `79f0a7e8ec1fc24d817acf1e5b6f99a0bed818424bb10e5bb438e5fdbb1e6511` | `60230da0d147a9b9b3be3b85f88433c8a61a1f0e20a0241eb225764036b9694c` | `4b460fd4e2f0ba8c1e156f11c262855f88d3c62a978c4dbcbe140a04403cda53` |
| PQ4K480 | `7c2758ebf2677dec7b0182ebff3d6d8af02396aa9bab3a4e750f50a61004d5b2` | `58d1501e0aa9030d7a926e5bf72ecf53f7d2157d178a06aa44f271ff8dcf51f9` | `49dcd55a30d37d4b02fa13a90b0bb410d97047cfc0102448e83d3067ac4f10bd` |
| PQ4K720 | `74feec2441fe6377e2412a06192b9866d4130e4ba815e29089b0f8d2c0279ca3` | `03bf23059c932b8d5cfc4b8ebbc032db8b8c3b12470d7b5eb31ca10213d61be6` | `8fe43f133e370e03411926944d81d38dbb5c924bf983471e5d2165c3abae6350` |
| PQ4K1080 | `0bb32c8c3c89b328a17992abb93a14ae2dbcf8d9f1beba555c1e69de7756b1a4` | `802d701953d769c3aa204c06265a5d467ebe07d88d7eb37e395df7150e1ebd72` | `7b97192257c9211f39526adb89f0b767c6bc1ba35bc810682fdbebca75b2cefc` |

| Context | Worker terminal SHA-256 | Outer terminal SHA-256 | Successful independent PID inventory SHA-256 |
|---|---|---|---|
| Grain1080 | `c722e22cf7f86fd36e031432be19b5295e32e781622e9873f0bb4892a8f000eb` | `c4a5953ff36d5e05a8698b035ee731c86d07fccd4e2f76d54af4425eff52e78d` | `de63ec844fb399e992ea514dc04d3cd1e2566a5504cb8b6750b3cee15f8c2376` |
| H264360 | `cfdfd5e3423c1833b5af4211a490d261c1281a361a05eee28ddf450bdc5ce69b` | `7b4bf98e0e7cc7a7c690c55c650432f3feb0f6f054da2faf8cbe5df44a41bf87` | `173e1444b0d99589439299d6a02cb37aed181104f56d9275e0ceb50385588122` |
| H264480 | `41baa376ba9b5331b11d751601f859284bed952236fb0e412a995eb14390ddd7` | `7945cebf7a12860485e5fd5f29490303dfb03b3fa722e719394ce5f7ba66ec74` | `cb8c85ab13be0a0860a61fa1e5b7fc521d57af598178a45d032117f1149ad06a` |
| H264720 | `856f9870809e8f154a7b8f543fecc1ee77fc8a88ac9a833799cbef2934ef4486` | `0834bc57b24ccfa043cf3fc027cde44d39ef9e1084838dac4770df007147152c` | `342ebe318a5c3b59f3c004be854a5643623efebcc3fada9d61d5c53fdd3ea946` |
| H2641080 | `ae1c673ccbc7bf6ed4529ea5dfeb55396838fc12345702aec414a8aaa6b66940` | `66aa8fe688e559317e1e93637504534e18703ef15eae973711a341a0035582db` | `dee9e6ca0ee1d2d96344907ca54de20fd7f01e4aa56ffd1c1de5969f813ea2cf` |
| SDR4K360 | `48c907c002821d00f24a6c6c3ea36447639dcd60ac31ccb52e7d5314d82b7c4a` | `3015cab37335f3a20edf7f14ac09b60a855d3b6af059108a4c6c7e581e56c380` | `c2b67fced6ba4093c12d4d92d22e96c2079e5b1195a2fe7c80af57d344c99392` |
| SDR4K480 | `f09c324f4dc355a76f13f33e92d486e6bb19571d8d1bbf8846dc28afd5bed882` | `b4ad9565102365e636aba3c23be2fa2ef7dc7332bfa3a5932569fcd612733b57` | `3a8d3ca7f22540018ecd69f28ac826f6218ce901481216e11c76a8f1bdff67cf` |
| SDR4K720 | `df3cc4123a74f4487b835536b336e192802f3e5cc6028259e9b6751b101df3a8` | `8f63f740e2a21a34afd618b38156c7a50b8ea8a694cb33ed26daea30a8dda54d` | `11cbda071c3a389f5ab2d5f7da5fdf3783e14715f03b4097c87ec471699b34f5` |
| SDR4K1080 | `c908be55d3e3609727ef825c4f17b40e371fd9edee5e1b64f4d0d012dce10254` | `85e4b5bb5a9714240c8553eccee3c4d3c57b30e87640484a7895b678f8402983` | `3d0e56e9ba775d9cb9159de4971a372a7bf62d1e5a6e0a1fe0a3379ef6b32d9c` |
| PQ4K360 | `2c07026ba8651d5d98df1d99b703c887026bec38942d8d3aa61f5d119e896bf8` | `8be6e5d56cd3953ee53c70fb78c0137cadafd262258fb5b5c5568d6597b18d4a` | `64b9cd19b851b75978c5073ee17058337ff6bd0c977079e903c676a4584dacf9` |
| PQ4K480 | `ac644646a28cc6ec95e8a6043316964f0dc7b0a9a74b91c8a1f5fde02be8fdbd` | `e699728a4219d69fa30812afd267b107fb0850d7908f441ca02d52a4423d1ba2` | `65f5779bd91dbceb59642eead7d984c06d217e43cb7fa1710c4217c7b089fe39` |
| PQ4K720 | `6aed982b6b8be57ee84d1bfa4cde62e4c44a74e27b6cecbc7db08c789b811b7b` | `77e861bd3e532449b8cb93abe5e5ac0d92399e255098e6e2d88000fdbf6f9582` | `6e78b04ea98617dc3638e2769088be67f733df7042e5a6abfe19244976fd9885` |
| PQ4K1080 | `094d56e0b42ad045f739090717b39106411baec700d02d2fac1e871830556ac7` | `17d46cfb0c6f0e1d3984b00a7a668e7d8193ccad55fe00fbce903ab9ee9e2457` | `10ddee86e27642ae9b6f0ec1e174e403d500f8124a4f8c1ebb976fa9b5316684` |

| Context | Exact nonce | Guardian/watcher/supervisor/worker PIDs |
|---|---|---|
| Grain1080 | `ebd94f5f444c439e88e0145b381c01f9` | 921008/921418/921419/921420 |
| H264360 | `a74c6fb057ad4a3f8be2ad0b7c695a62` | 987994/988094/988095/988096 |
| H264480 | `04499fe60b27459ba03c1a076783cf6c` | 989198/989332/989333/989334 |
| H264720 | `494a4d03eb3b4ffcb087790178f9b063` | 998210/998310/998317/998330 |
| H2641080 | `94ef4b11ddbd49fda23912fc807f82ff` | 998614/998994/998995/998996 |
| SDR4K360 | `dc178473d52c49dcbed2eb0c26666cdf` | 999431/999537/999538/999539 |
| SDR4K480 | `ca628098883a456b8abcec4ccb3e2788` | 999763/999864/999865/999866 |
| SDR4K720 | `312f2a5a59c24cffb9c9478711ab465c` | 1000329/1000520/1000523/1000524 |
| SDR4K1080 | `63345fc22bd34f5196f6bcc02f254af8` | 1001145/1001245/1001246/1001282 |
| PQ4K360 | `938006d8ae234a359e17c3569422cc80` | 1006164/1006281/1006282/1006283 |
| PQ4K480 | `9ec8073a34e14d34bbef11680ef2b3ab` | 1006529/1006639/1006640/1006641 |
| PQ4K720 | `48de3e1f904d479bb69bba2e812043a3` | 1007049/1007191/1007192/1007193 |
| PQ4K1080 | `8b5f4a23a6b343bcbedeb31d4ec283fe` | 1007613/1007726/1007731/1007732 |

## Failures stay failed — successful later admission is a distinct event

The old Grain1080 namespace predicate wrongly required one IPv4 header and
empty IPv6, refusing before census. Failure final SHA
`78fb337791d86b7efdfa4c8c0962a8d6f3a4708ebe5872f3f271fe9bc85fd466`,
stderr SHA `6d0b15ec675eccb513eac2648d070fb2ed61ad693ce9e468238f4da7da2a1786`;
canonical failure receipt SHA
`ad89788eec0a3276eb531e0d2e0e92355ac1e7930cc9fff5093434b01d8d874c`.
Partials were exported and exact owned cleanup independently verified.
Root's distinct non-media observation found empty IPv4 plus two exact inert
IPv6loopback REJECT/NONEXTHOP null rows; observation SHA
`4fbb593e996e92bf95d46942f5c0e86433b1a768e1971d24ff74cd9911802e5c`.
It is not recovery of the failed worker's unrecorded exact tables. Corrected
source keeps changed namespace/onlyloopback/all network denials intact.
Old source/failure records remain frozen.

Earlier auto-review refused before CreateProcess; root local source-mode
refusal preceded manifest/SSH. A contemplated pure edge fixture also refused
macOS RLIMIT_AS setup before fixture. None is parser failure or credited
control. Grain1080's separately approved corrected run is its sole success.

H264360 and later PQ4K360 each reached one actual bootstrap SSH and refused
`known pressure threshold` at snapshot, before leaf mkdir/guardian/source
writes/media receive/census. No result exists for either refusal. Each
sequence stopped, then root separately authorized fresh actual admission
for still-unmeasured cells; no successful census/controls were replayed.
Historical refusals are not current permanent blockers or relabeled passes.
Threshold category/numerical sample was not returned: Unknown, not inferred
CPU-specific pressure/memory exhaustion. No threshold/cap relaxation occurred.

| Failed admission | Whole seconds | Host final SHA-256 | Held manifest SHA-256 |
|---|---:|---|---|
| H264360 historical |0.999660708 | `f9c32a20c063945fa8b4538c845bcda59e793258ff0c10247326b5c662e50de2` | `b23252e2e9102b81b0704dffe3415339a85376bc5c80c5dbc83731ef751a7b07` |
| PQ4K360 historical |0.602091208 | `8cd220a4bb708e325ddd35aef2ad194087a32307bd96e5b232acae5f532a66e9` | `ef0946e52f3ad93c049dbc6df8762d8938253e55327f03d234c102bc3b64c961` |

Each298-byte traceback/control SHA
`b745a4f6dca7bc97d179619d90854dff164342434da35117898d0d9fe22a0e9b`.
H264360 canonical pressure receipt SHA
`070f906f008c59abe41b211b223ee15618b82b43753f44d94d7220000724e811`
remains unchanged. Conservative `uncertain_owned_leaf_retained=true` is
not evidence a leaf was created; `cleanup_success=false` remains intact.
There was **no independent remote absence inventory** for either refusal.
Local SSH children were reaped; no arbitrary remote-resource absence follows.

## Retention and remaining acceptance — hashes are not a new durable capsule

Raw Grain360/480 results may retain an old session capability and stay
private; only hashes/aggregate facts appear here. New Linux receipt/result
lineage is sanitized before transfer: original raw acquisition receipt
hash differs from copied sanitized receipt hash, with only named
`$.provenance.session_id` omitted and no value published. Original TS/
probes/playlist unchanged; new descriptors/sanitized receipt mean these are
not exact-raw-result replay claims. No credentials or capability-bearing argv
are copied into this document.

New raw/source/receipt snapshots remain agent-owned private evidence.
This continuation adds no TS/binary, attachment or new permanent tooling.
Git readers can inspect scoped hash/metric statements and historical #737
parser/control snapshots; they cannot reproduce all fifteen later contexts
from Git alone. Root's architecture effort retains the thirteen raw Linux
bundles, original source receipts and failures privately, and provides the
sole independent reviewer direct local read access. An additional private
raw capsule is an optional retention improvement, not a prerequisite for
this scoped ledger. Independent review and the current effort gate remain
separate from the header measurements. Existing #737 snapshots and private
audit-byte asset stay unchanged. Owner is root architecture effort.
Original input expiries are unchanged; runtime deadline
2026-10-03T04:42:56.098867Z is unextended. Retained output does not authorize
producers or prolong expired operational sources. Root released its runtime
lease after all thirteen successful Linux nonce cleanups; no new grant here.

No retained internal header context remains unmeasured. All original S11
real-film/media1/public Create, entropy/conformance/closed-GOP, native/GPU/
device/physical/fidelity bars remain open. PQ-source H264 scaling is not
calibrated HDR or tone-map proof. No producer/probe/decoder/source-census/
old control/unit replay occurred. Original S11 and main promotion remain
open; no gate is waived.
