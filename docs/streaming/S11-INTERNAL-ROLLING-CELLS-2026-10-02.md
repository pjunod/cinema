# S11 internal rolling cells — sixteen measured contexts, not original qualification

**Status:** done — evidence frozen · **Written:** 2026-10-02
· **Owner:** gpt-6.1-sol, agent:/root/s11_next_cell_sol61.

Companion to [the canonical codec plan](CODEC-AND-GPU-QUALIFICATION.md#54-m3--q8a-does-the-rolling-grid-drift).
This is a sanitized digest ledger for retained private evidence, not another
status board or a replay instruction. It executes no runtime change and
admits no new codec tuple. The documentation branch began at effort
`45a123d59fdeeb4337bacd367ddcd35df027ec3a` (tree
`51f940cf6cecc1cc85a8a6a92088cbc56351a377`); these media measurements are
attributed to the older measured source below, not this newer effort.

## What the sixteen cells establish

Sixteen distinct internal manager → actual producer → shipped handler →
headless Chromium contexts completed on four separately identified
synthetic inputs. Each ran once successfully; repairs reran only failed
stages. Each fetched the complete contiguous `seg00000.ts`–`seg00034.ts`
window, advertised 70 s, and presented at least 60 s at real 1×. The offline
stage independently consumed those fetched bytes and bounded raw packet
probes, then validated their hashes. It started no encoder/server/browser.

Every cell has 35 × 48 = 1,680 video packets at 24/1 fps, EXTINF
minimum/median/maximum 2.0/2.0/2.0 s, 35/35 first-presented-packet keyflags,
and no measured packet-grid drift. Maximum grid and packet-span/EXTINF
error is 0.0006666666666666666 ms (0.000016 frames), within one actual
frame. Grain720 alone has two extra internal keyflags: `seg00006.ts` at
packet PTS 13.958333 s and `seg00018.ts` at 38.000000 s. Extra keys do not
establish drift or justify changing production GOP flags.

**How to read it:** these are 16 internal cells / 4 synthetic inputs /
0 original fully-qualified public cells. Keyflags are not independent
NAL IDR/CRA proof. Original media1/public-create-route corpus, the two
hash-named real-film censuses, applicable GPU/native/physical acceptance,
quality-per-byte/concurrency/fidelity and the broader M1 content classes
remain open. The PQ-source cells emitted software H264/yuv420p through the
observed scale graph; they do not prove HDR output or tone-map fidelity.
No production flags, cache identity, release, deployment or gate were changed
or waived. No successful unit or source-generation stage was replayed here.

## Measured source and tool identities

| Identity | SHA-256 / exact value |
|---|---|
| Measured source | `fb4360792a3ae077aa73a40ee30cfcdd024ff445` |
| Measured tree | `cc94375018a2fb569833d783dbdc9da93336fdf0` |
| Source-only archive | `5a5b30589bd78ba460716686a90827d93a0d75b38ba4b624b9445008b774a0a3` |
| Linux ARM64 lab test binary, not release | `656e75891579d9526de544874f92651649ddf8f85071413b0bb207b4aabc524a` |
| Runtime image | `b7bc6f794e9d6aebae8fd54c32c513c3507173729e40c1b5048097e09e23d303` |
| FFmpeg 8.1.3-Jellyfin | `10a017e55452a171a8e24287caabfbc24ce3a8d6182acdedf86a000ac8f3e929` |
| ffprobe | `26f9a2e0b353af160ef75fcbd12d5d117cbac014a323626af5b9fe06ae79dc93` |
| Chromium 154.0.8037.57 | `749f4ecabc5840aa66498ef43a1b5425e866ada6479cd8c6d4ce2141145bbaee` |
| Unchanged acquisition launcher | `309ad1ccf456b11f5985a9bf257a8b62272013d24cdef0b6ace744644d72906c` |
| Actual PUT process observer | `e864f2ef05fae85fb80e8b727ac7499d6ec3f326a5638d58dfc852637aaf0784` |
| Unchanged offline collector | `fd50ae5aef1869c217a6cf0673372b84511248b34be709cf63ab9cebb0084f39` |

The private observer retains actual full kernel NUL cmdline, held source FD
stat/hash, executable, PID/startticks, parent, private namespace and actual
parent-owned loopback PUT listener socket. It does not infer a producer from
a retained directory name. Capability-bearing argv, source paths and
credentials are deliberately excluded here; witness and graph hashes below
are locators, not independently reproducible public authentication proof.

## Four input byte identities, not interchangeable class labels

All inputs are separately identified 70.023 s containers at 24 fps, without
looping the ordinary 45 s corpus. H264/Grain use 1920×1080 pixels; SDR4K/PQ4K
use 3840×2160. The SDR4K/PQ4K source receipts additionally retain decoded
mono AAC/44,100 Hz counts; those audio facts do not certify output fidelity.

| Input | Bytes | Video class | SHA-256 |
|---|---:|---|---|
| H264 | 114,979,608 | synthetic H264/yuv420p | `ad613aea680e3908571b8d9f5d1129a050c7a28b685c1cb9616ede01c8e7922c` |
| Grain | 1,222,841,401 | synthetic H264/yuv420p grain; unspecified colour remains unknown | `df310256db516c37559f76f0e5f3945609b5c2824b1c548912731de1f49e65e1` |
| SDR4K | 108,262,731 | synthetic HEVC Main/yuv420p resource variant | `673e5095c2217552733c81426adf371e0a8427a94d15c5c459470a96d31d6e20` |
| PQ4K | 145,219,272 | synthetic HEVC Main10/yuv420p10le, PQ/BT.2020/NC | `9b9135ef50bc9e03ea8f5a4cd5519801f0e5e6351ca3f54ddb13ebe6940c647d` |

SDR4K retained its pixel/audio/preset/GOP recipe while explicitly bounding
encoder pools/frame/lookahead threads, detected CPU count and shortest-buffer
resource semantics. Generation passed at 1,623,146,496 B cgroup peak under
2 GiB. PQ4K additionally bounded filter threads and used a separately
admitted 3 GiB source-only generation ceiling after 2 GiB OOM failures;
its peak was 2,526,400,512 B. These are distinct resource-variant source
hashes, not canonical generator bytes or identified films. Cell caps did
not increase. No inference that 3 GiB guarantees future success follows.

PQ4K eligibility combines the retained 1,680-frame/full-count probe with a
separate once-only first-24-frame and SPS/VUI diagnostic on identical bytes:
10-bit pixels, PQ transfer, BT.2020 primaries, nonconstant-luminance matrix
(actual SPS fields 9/16/9). Missing stream-summary primaries/transfer did
not mean absent bitstream metadata. Mastering display and CLL were
unspecified in the recipe and not sampled; their values remain unknown.
This is not calibrated HDR10 fidelity. Full-probe SHA-256:
`5ff09b5f6878be747e5b4a7f50e6a08bdd3cd9e814e3f44cca6b44334c64c400`;
frame diagnostic `f75c41272fe43570b67e389afa4926491c99777ffd6967163033a3bce2c52457`;
SPS/VUI trace `c4fce5ea25522cc8cf8b001468bb2d4909b47302d9c4e8df2b4459ba96973a94`.

## Actual geometry and presentation per context

| Cell | Probed output | Real 1× progress (s) | Segments / packets | Start keys / extras |
|---|---|---:|---|---|
| H264360 | 640×360 /24 fps | 60.396334 | 35 /1,680 | 35 /0 |
| H264480 | 854×480 /24 fps | 60.146334 | 35 /1,680 | 35 /0 |
| H264720 | 1280×720 /24 fps | 60.063000 | 35 /1,680 | 35 /0 |
| H2641080 | 1920×1080 /24 fps | 60.479667 | 35 /1,680 | 35 /0 |
| Grain360 | 640×360 /24 fps | 60.229667 | 35 /1,680 | 35 /0 |
| Grain480 | 854×480 /24 fps | 60.396334 | 35 /1,680 | 35 /0 |
| Grain720 | 1280×720 /24 fps | 60.354667 | 35 /1,680 | 35 /2 |
| Grain1080 | 1920×1080 /24 fps | 60.479667 | 35 /1,680 | 35 /0 |
| SDR4K360 | 640×360 /24 fps | 60.188000 | 35 /1,680 | 35 /0 |
| SDR4K480 | 854×480 /24 fps | 60.063000 | 35 /1,680 | 35 /0 |
| SDR4K720 | 1280×720 /24 fps | 60.479667 | 35 /1,680 | 35 /0 |
| SDR4K1080 | 1920×1080 /24 fps | 60.438000 | 35 /1,680 | 35 /0 |
| PQ4K360 | 640×360 /24 fps | 60.021334 | 35 /1,680 | 35 /0 |
| PQ4K480 | 854×480 /24 fps | 60.271334 | 35 /1,680 | 35 /0 |
| PQ4K720 | 1280×720 /24 fps | 60.354667 | 35 /1,680 | 35 /0 |
| PQ4K1080 | 1920×1080 /24 fps | 60.229667 | 35 /1,680 | 35 /0 |

The actual graph is libx264 with `scale=-2:'min(H,ih)',format=yuv420p`.
A complete actually served playlist revision was selected for each census,
not a derived accumulation or a guessed snapshot index. Complete playlists
share SHA-256 `2dd773c87ced6fefa63fd8370e79eec901855cb6ec3c06487ae0a71a829d2e0a`;
that proves playlist text identity, not media identity.

## Digest ledger — retained private objects, sanitized publication

Each cell below names its acquisition/census private root basename so the
owner can locate the raw evidence without publishing an absolute source
path. All values are full SHA-256. Acquisition and census finals bind source,
process witness, original deadlines/resources and terminal receipts; packet
receipts bind all fetched media hashes and raw probes. Equality means all 35
fetched, producer-retained, served and census copies matched bytes/hash.
The object-manifest digest is a documentation-time read-only cross-check:
SHA-256 of UTF-8 compact sorted-key JSON of the ordered 35
`{name,bytes,sha256}` objects. It is not a rerun of an acquired cell.
H264480 has no separate equality JSON; its contemporaneous byte comparison
is retained in the private narrative and the same object cross-check is
reported here rather than inventing an old equality receipt.

### H264360 — retained successful context

Private locators: `codex-s11-current-4133bbc752314a9e873b7e7e` · `codex-s11-census-7404d2e653b4464b87dc4df3`.

Acquisition final: `e54b78cc5def4e103d62568ca1fc458a21b86a5ad8c07c4fdbbe57be5a844cd1`.
Census final: `230e8992aeccf5749cbd4689c1f833f2f1d5e600015cc39feb0d8b1598a0c4fb`.
Packet receipt: `96e056c6c11d1b178e3e075da282fb710fccc0f5349f18110096b1a4ba4f8dbe`.
Actual graph: `14f7b580bb90cbf011330a48afda16488bc2436501b80e9a06290cb95d217210`.
Private NUL witness: `80759aba058a6b0225ccbc2cd1a374bef87f5d36b9f11c985ad4c6ccabb7a5e1`.
Equality receipt: `eb8bf5b92144d5b3bf90098515b7aac1a0effb2596af371feb77dc21d43d1b33`.
35-object manifest: `3f684bde7f2840fb7e0e5dd0c221fcd4ef3fcda3b722cee51b74070a23db57fd`.

### H264480 — retained successful context

Private locators: `codex-s11-current-ffd9d70bbbe54f1aa9cf3126` · `codex-s11-census-0c8b2b6d484d4889a051f3ba`.

Acquisition final: `1c0eb816d15847d41f794cc4130e5fe6141ba24bd9bd91eb4716ef126c30ff3c`.
Census final: `f242e78dfffe5cff6bce00639438db2c079a37200c6d8df4484396ad486b4c4b`.
Packet receipt: `ca6a96161b3129f58710269b8cdaff784f312d7217e6e3d6b045d35bd06ebc80`.
Actual graph: `7abf5943efa7020d8008383d37773626f83b44a822661d295573bf8638576801`.
Private NUL witness: `f03f7bf9be8e2db3171f7ba42239a294c5b15b51b5d1c98ac8a6d1d3346e7ca1`.
35-object manifest: `6b5378b1700ce0e75f702ecf11d7073b2768959d86403643cb707335f33b56eb`.

### H264720 — retained successful context

Private locators: `codex-s11-current-ea1ef6c1850942888d195880` · `codex-s11-census-3736cece9cf2498d9681e5c6`.

Acquisition final: `1b65cb09d5cdd990fad84a8848011df630f6670010c98c67de326756c690629c`.
Census final: `18b33b4040d0a22fbe1521b908d0b98cfdbc773fbdb2b5a16ba1bcbafccc9d99`.
Packet receipt: `987f1ad1c5e4a99d06603e2d002e2e80ed16c3fde143e440aa1cb60c58907cea`.
Actual graph: `0a913c0eab5d2102b0558436517181f942afe71bfc0a76a28859b077e3605c05`.
Private NUL witness: `a7ebb03cfa6bcf7347a8e98f8197b237c437876b4f8190da8d2efd96760ca44f`.
Equality receipt: `ab33c5f266a4f553edfe600473908dea6560735a56ddc595aa38a9b68733ab0a`.
35-object manifest: `3923b73b27cf1557a5ed95291f139c37cef3fa379854a56c37c3110c182cef82`.

### H2641080 — retained successful context

Private locators: `codex-s11-current-f82b8868385341c59a54b4c5` · `codex-s11-census-75ef7622da2d43148bccaf90`.

Acquisition final: `60e1282bda22ce9d6c703a3e5a8803496eca4c489b75d13c22c1d650347f3f24`.
Census final: `288b8feff0e2c2682d5f098f6f9c8f1c89bd05822b207be4fdb3b0ccbe3757a6`.
Packet receipt: `625ee5655221981030f8c76043768c57530271ecad38e94a23babcab389f10aa`.
Actual graph: `a84496be09de38bdfb6dbfb3cae68d7826676c72f12055ad5a799efbe2095684`.
Private NUL witness: `0e39866cb09cf1c1f67253ed4538ecf31aa05f408011c6148440161db413ed2a`.
Equality receipt: `c95657dd4d865c2deb69d64cd3dd741adba65a968a48f743d0bc479a07101343`.
35-object manifest: `b83db25cc46e5876d37f49d17beb90b62397156a6b7075c59aeaf953da1aca4f`.

### Grain360 — retained successful context

Private locators: `codex-s11-current-64291f51e293498b8a309d30` · `codex-s11-census-00081c0944624f19931b5a5c`.

Acquisition final: `e3a211006af34c126d52131cef3276908a23f587f7a400455ba151a95b05d87c`.
Census final: `be08f8bdfc66e5362ef93c3eba4717758ecf859e6917d2736c2ebe05af65e79b`.
Packet receipt: `b2b2ecd8b3196fbdbaeebf6048894c77a2ff9e16a8ea0b95753e856a1be73f9d`.
Actual graph: `62730d9709bd555a543713beab824adf35642ceb622de0b153d140f8c498aa29`.
Private NUL witness: `ec15b5a9efc7c11935e4ffd55dade09fbe191deda41148ebd17c861be66114f9`.
Equality receipt: `daf452b8bcfa5ab97a94de78c49b550ba5fb9117736fff084b1d80e04ea89e91`.
35-object manifest: `6980339e595bb432695d27ae562476468aff8e6093a97096646d1a5faa5290a0`.

### Grain480 — retained successful context

Private locators: `codex-s11-current-3e7e177af656406f9956ad8a` · `codex-s11-census-321d7d5cbde24fc49fbf491a`.

Acquisition final: `c212fbab9cc68ce784fe78b0ca8a4d670043f08de149e5f01abd01d6071c34c2`.
Census final: `65430dc94aef5ebe261035a205948d8403b7b21340d8b03320ec6588b20a0e23`.
Packet receipt: `ea098b1f847c510d4770366434a03ee1f91b800e308ce4dea3fb74415151eb53`.
Actual graph: `b65352af2fdb699d3763901acdd0f6bc49efd08405cc43885cac9dbe2639197a`.
Private NUL witness: `8fec291fd646d7967c3580d836c1efd0106ae9e11dea829578da6cea5e1d20e5`.
Equality receipt: `467f9b89e9c89c81a5561e5a1e6a81d20e09b712d82b69c99114f82b2cb74200`.
35-object manifest: `dbb53d8e0578f5c61877970577e8928d4bae9ef88ecd9e568f070b1ecc4991b4`.

### Grain720 — retained successful context

Private locators: `codex-s11-current-3e349373fed3405c8efb5350` · `codex-s11-census-c9d10b5d892e45bbb004efd5`.

Acquisition final: `6156e648312aad38d8d86a20b83e74375f911815150df56dae23673d878af1da`.
Census final: `7c1d863ca5e3cff08c838f01eea8b79149083a76f23caaab0fc14d700ada89cc`.
Packet receipt: `6039c85c357b7a6d2129b69677e8f5881a8489d214cef71b455f17bb2c88a18f`.
Actual graph: `8f269e81592e66a36a81ed15f764b09177bdffcf79ff449ba5794b3b46a439de`.
Private NUL witness: `221089b320a8efa58d3e38618c2d7fb8ac693cc1969aad0bb968f279f6f5d6d9`.
Equality receipt: `7601018a60843c26c3743aeaeb8d82d0f4f0855613ea574425f18238be3173ed`.
35-object manifest: `eb77d9c705fefdc0b81fa12ba441fcd757f83cd3be092d7a2c7b831290181b34`.

### Grain1080 — retained successful context

Private locators: `codex-s11-current-1f5d10428a884fab95688db5` · `codex-s11-census-23e81099dfad47308c68c729`.

Acquisition final: `b1ac86c83f379c329b47869a4d65908f2f4ddcdc384ad4a704b72ab5d43db668`.
Census final: `f488a32a4c54634edaacb36aa155daa6920281780bf9ad3dba68640be925b71a`.
Packet receipt: `d5c981365fd3d9c54c46f353f2cfd732d28200ffcfc24f2f1d99848adb66a3b6`.
Actual graph: `96949a59748389087e77d088172c4e47c674ddec22d851de214517ab704292d2`.
Private NUL witness: `f79c0369b305c8de5b8be75fc13e5b9bf0bf0274bfa0091ca9b984ba1bf82a67`.
Equality receipt: `82805df00817935872c246a90b4ba5557c262ece9648a2c112a5b5707692397c`.
35-object manifest: `aa5194dab4450dbe1e71cd39b59485be24f04bf9b69f3ff5bd85c3fa1f106c6d`.

### SDR4K360 — retained successful context

Private locators: `codex-s11-current-a36634531f404f0fa18f4a0f` · `codex-s11-census-696059ae629a43e2b14cdfb1`.

Acquisition final: `e8b7c31d8c4f586d1fd262a5a15f019e0ea53b03fcdc0bb194a86a923c31959c`.
Census final: `248ca208979370476707f605d59781358313745c7f933faca0db814aa75bd2f5`.
Packet receipt: `0905251a218fea7ccd0d77acec2f32aeeb6aa207975b0b5436181e34bbdaed74`.
Actual graph: `a9ac2fdb45b21e1ac6413216005cbc263baa052b99c9e93c22618f4c34cd34d9`.
Private NUL witness: `c93a19db48a6476c846ff50c1efe100bbf5c5a9af1751d4d974b1eeec7943005`.
Equality receipt: `85a8d6405b991b5b8cf1937da8985ba31b97823a11ace76e2572cf72b5ee7cf4`.
35-object manifest: `b672c9c8b3529dfdc9a30dd0ef3b42d8f4868b390231685745da1e4a69d21bd4`.
Independent terminal inspection: `e42c8c9fdc8b9405ad72fa4f0279e4db3fedc44759bdb7740163039902834fc3`.

### SDR4K480 — retained successful context

Private locators: `codex-s11-current-b2922ef6c862411fb66c5d33` · `codex-s11-census-ef926403b04f41db9c3f4a07`.

Acquisition final: `36c6be8f995547b284fc28439fbdd9365a466bda43af3c76fb941f3bb0a8e691`.
Census final: `bb37514c28f451d3eacc5a37e2fd8ff974bba8ea14482763a46ab417ca9acab8`.
Packet receipt: `da42ce13b0ed74ddf306d6974d749d663cb43ec5be507dd0447829966b7c7c6b`.
Actual graph: `cd463e5c8bdf0413f5467690683dcb2fc8caae70bcf16687f0ba8d80eaa41d94`.
Private NUL witness: `88ea303b668d7e0975604f6803f79bc8420d31ea3d145f24509cbf3e5501dba0`.
Equality receipt: `6b47e3ea01e3390126a0c8edbb9f5f0aa7c999914367c9028f00bde692d92b15`.
35-object manifest: `8e9a34b092e34e1b1570cd54453ea63dadb96ff973a38f0075acf61943c3d658`.
Independent terminal inspection: `b928c5a0e0826486a666cb86658f465b5ee17bc5b6d5d28ac9eb6aa73fd49023`.

### SDR4K720 — retained successful context

Private locators: `codex-s11-current-a73682e9fde342c7b1ebd4c2` · `codex-s11-census-ef1e45e573014f3e9e792adc`.

Acquisition final: `736446ff6cefeba751746ddc75bbf9aaafec6c2a1c3946e9a9eea360cfc478a2`.
Census final: `84cc24cfd911d9594aa79568139470f8fd153f8ebcc4938985ed0ffda95089ee`.
Packet receipt: `aa767bfccc7633abc7dde8090175f7a2eb29c0653ce19ef2b062c74a21c1a073`.
Actual graph: `82489836ef83cc6691632c23ec4d235d4c21f97b6cbfba88770db0b8d7778ad9`.
Private NUL witness: `e885beb32567ec35b55ae1e122db7bcf3e158f56741b9f354d735817ef2a6b4e`.
Equality receipt: `c801d3245e5af47b649f86975f3dd06115e61e4774b38b0e6d79bf781719065b`.
35-object manifest: `64b948323e9179dc1868ad036f3209179413bc9d151b3f90aa2c4985164a5c5d`.
Independent terminal inspection: `c12e66eca72a44573fc77267391b310cfff659225179570c905646d246c3f333`.

### SDR4K1080 — retained successful context

Private locators: `codex-s11-current-805d7b697f7b4cf796cf7d87` · `codex-s11-census-582369ff9b574a7386ea7d7b`.

Acquisition final: `86430024394e9a839ecb956ea8f4ad33e70267e6846243d4d8f6bb407321b544`.
Census final: `e8c6c8d69fd8bf88237b4e6f05feb64404634fe21aec08a14a46e09230727c45`.
Packet receipt: `70cf5f86aa4aa9e544c7374b5e4da2828192c5b7edd2eb0311979aa42e7d2f81`.
Actual graph: `6741663d9a5210419cea2fea685f90e1060b8e8604a5d6fd8cb5c91d95eb6590`.
Private NUL witness: `d7c821cb6fa5c7b1123a71702b8c618d5284c07abec5bafc699876f5d24dd9d1`.
Equality receipt: `eae50a6b4e9140ac81eee7f12590e5b615aa39c5412df7adc3ce77fe2b39b778`.
35-object manifest: `d43fdca439cb24f8cb7fbe5a0fc4fc6bce8c9a55c14186b80bfe37e76993da69`.
Independent terminal inspection: `085b12880460794ab8e94ebf92c16e8566f80f056fc46c658529232b3189539b`.

### PQ4K360 — retained successful context

Private locators: `codex-s11-current-398367362395427780b6c82c` · `codex-s11-census-40eef8e69c1c4e6db7918ad0`.

Acquisition final: `7cb231525520fa65e5214a0cbf96e81fec61c4e5a6a92a268ed7460cf07ba219`.
Census final: `5dc26554d1c83989c29a41d0cf2685fd88e02df5f20939a01fd56f110c04ac54`.
Packet receipt: `0b8482d25e91c0eebcf30fb3df594b2a62e3624311225b3fdcbfb75b6d0b687b`.
Actual graph: `6c0e6cdd644d47c9fea4d4760cb9a4282964d2b500e50618c4cbf40a2537b53f`.
Private NUL witness: `a30c21aad9665db6b1046f74818498cc6e93bdbc569b67f764df4745d2dc11a9`.
Equality receipt: `ba47647867ff00c6b7e11da88ccd3068babfb0a1b0e49980b9268e84da725061`.
35-object manifest: `48bf2b7aa64e70fee56f7f9364b0b809f72002b5389f43b312703c5c9451da6c`.
Independent terminal inspection: `605b8f49438ad4f649e7d612d61b4e331122d797f334d918793e482818884ff0`.

### PQ4K480 — retained successful context

Private locators: `codex-s11-current-bad3ea2934014e538a0df925` · `codex-s11-census-7b702fae6ca44fe49fac3546`.

Acquisition final: `34ed87457269537de25d4b61444649ad739f0b01b7de6fc82869153604064ef0`.
Census final: `45dd30efa39f048b3a073269d35feeead9953ac9c094b455759aa7267efc0be8`.
Packet receipt: `be861268306cf2a0b5eaac1838b93d2a7b17ace5712274e45fb9789a7f6e9f36`.
Actual graph: `aa109bd349209088261810c1c137815c622753cd8c82ae1bb3c80c8111d54328`.
Private NUL witness: `33725a9996cd255b429fffdf2c78f04a46f800b56a98b9ff823c22cc04f9ab5e`.
Equality receipt: `b22e0b89827f38cf97a47017161d2e56a0f4504e7189ca05781c508919f14d7c`.
35-object manifest: `64879a7760e5ea10029402ec8d97559b09fb3dfdeae1f6ecc7097a8bd7bf9ef5`.
Independent terminal inspection: `ddb940832bc187ed2a46f53c441cf6042c911e7d3bdca12ec3ae874dbbdc5551`.

### PQ4K720 — retained successful context

Private locators: `codex-s11-current-cfe5ca028bab45bdb69f389f` · `codex-s11-census-151d0be0540d40f5af113e1a`.

Acquisition final: `9a1ec5542d77699ea61ee62e57eb1aa6a7d9d4347b897db757874d0908c2e7fd`.
Census final: `2697a1fcbf75b8b65d40743191d1967a3b61e1b272df89817989422a116a5a59`.
Packet receipt: `cb3c5239913ffba442bbc3fce186c69332d5108cbe050cf089798d64350fab93`.
Actual graph: `4fe3e59706214a194f6d0672074040e6f909cc6a00c6cdb2086c8b815f162fac`.
Private NUL witness: `b3759bece7c50d7ed2217561357eeefbc09809a918560ec8d36c3f72c8ba5f79`.
Equality receipt: `0e4ea045513cd22c3a50a64d207739076212dfef71dfadd0c44bcffaaadee56e`.
35-object manifest: `d7023b07b704c9e90f455598c2ab29078dd6e06901a122e23c1035949c0c7d42`.
Independent terminal inspection: `a63c80164cb4bcea1944d6de8cab161270b63ee4949ef077b4d090af5e90a706`.

### PQ4K1080 — retained successful context

Private locators: `codex-s11-current-9a8c96aaa5d54bccbfa4ad04` · `codex-s11-census-bcd9d8522076499e91deda6e`.

Acquisition final: `142ccc9c2b381484c541f02206d2290094e629b870ab8a3fd20bea46868eb236`.
Census final: `e697da07d11b0d1f1472e994618f571c652354ef29f2b50c7e489b618cf01900`.
Packet receipt: `6b32c517ee18556ff21b73c62bd3c0bf6fab563b11cc945399032b42563e74ac`.
Actual graph: `0e9b95ac538f234732aa51b76cf8e0ad9e656995692123a78007dcf705f5ef97`.
Private NUL witness: `782bb287f2b173bc2f3e9fc56fb66782d98c72cdf19230af15d0434cfcb590a4`.
Equality receipt: `f16aa71605842fbf08ad57800e024fe6841f02e153bda7a3224d3acfd126bb70`.
35-object manifest: `70da78be840124675b2caeacf4fa83efa6e6a90135d324334b17986759811402`.
Independent terminal inspection: `9d5f5fc20472a42a1a6ecfc37ac65bd02caf7d5871e7d28ac31ce399ff799c15`.

## Resource and exact cleanup boundaries

Every cell acquisition retained 2 CPU /2 GiB memory and equal total swap /
256 PID /network-none /600 s outer, 585 s inner /64 MiB logs /512 MiB media /
4 GiB nonce volume. The lab test runtime used its explicit owned 8 MiB stack,
not a production stack change. Offline census retained 2 CPU /1 GiB and equal
total swap /64 PID /network-none /690 s outer, 600 s collector, 20 s/probe /
512 MiB evidence. Fresh Linux UID501 mode700 volumes retained strict owner
assertions; Docker Desktop host-bind UID0 mapping did not weaken them.

Per successful context, exact preflight/prepare/capture/census containers
and both nonce volumes were removed; all temporary watchdog/volume-expiry
owners completed terminal0. Independent exact-ID/nonce absence checks and
explicit Docker handback preceded the next lease. Earlier failed attempts
have separate cleanup records, not invented successful cell attribution.
The final PQ4K1080 terminal digest above binds all four exact container IDs,
two volumes and six terminal/absent watchdog/expiry owners. The unrelated
old exited container and original runtime/input expiry owners were preserved.
No Docker lease remains and this document authorizes none.

Runtime expiry PID18248/deadline1791002576.098867, SDR input expiry
PID57282/deadline1791027832.503485 and PQ input expiry
PID76398/deadline1791032662.252828 remained unextended at final handback.
Grain input expiry remains 2026-10-02 23:32:56 UTC. Retained evidence is
private and finite; do not treat these hashes as indefinitely downloadable
artifacts or extend an expiry through documentation. Raw copies/witnesses
remain untouched for any separately authorized future NAL/fidelity audit.

## Failed attempts remain failed

- Original H264360 `710d32da5dd9450eafc68fe6` observed presentation but
  captured zero argv witnesses with a filesystem-output observer. It remains
  aggregate failed; the new PUT-bound acquisition is distinct.
- H264480's three preliminary census contexts failed log or strict owner
  admission before packet probes. UID501 Linux copy repaired the failed
  admission only. H264360's first census chose nonexistent snapshot0003 and
  failed before probes; only that stage used the independently selected
  complete0002 revision afterward. No successful acquisition was repeated.
- SDR source OOM partials at the retained 2/4 GiB bounds are ineligible.
  Pools-only limiting still observed 73 threads/OOM; detected-core2 lowered
  threads to21 but also OOMed. A bounded diagnostic implicated shortest
  buffering, but these observations are not a general causal guarantee.
  The distinct shortest-buffer1 SDR generation supplied the accepted bytes.
- PQ source27001 and filter1 source96547 both OOMed under2 GiB; partials
  remain ineligible. Source25200 encoded successfully under separately
  admitted3 GiB, but its decoderthreads1 full probe timed out20 s and that
  aggregate remained failed. Failed-only threads2 verification46344 lost
  tmpfs raw results; its duration was never credited as a pass. Persistent
  verifier44607 decoded1680 frames once in13.927766798 s, then refused missing
  stream-summary colour keys. Metadata-only75532 resolved that narrow
  reporting uncertainty with actual frame/SPS facts, not force-tags,
  remuxing, regeneration or replay of successful full decode.
- HDR360's initial read-only expiry assertion failed before any container;
  its failed-only assertion repair read the actual owner file. A later
  helper overwrote that initial admission JSON with post-check facts. The
  initial bytes were reconstructed from already retained raw tool output
  and matched the recorded initial hash. That storage flaw is disclosed,
  not pristine-original evidence. HDR480/720/1080 initial admissions were
  separate immutable files and post-cleanup admissions separate again.
- SDR360's read-only cleanup diagnostic mismatched Docker absence-string
  case; only that diagnostic was repaired. Actual cell/census stayed once-only.

No old failure borrows a later success. No current-effort qualification,
public session, physical/device/fidelity acceptance or full S11 closure is
claimed by this evidence-only continuation. Original canonical acceptance
and separate independent review/current-head effort gate remain required.
