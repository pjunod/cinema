// CPU-only L5 semantic controls. No GPU/device or licensed reference required.
// The oracle is the unchanged full-frame mapper (CM v4.0 or CM v2.9) applied to a
// separately cropped image.
#include <assert.h>
#ifdef NDEBUG
#error "active-area controls require assert()"
#endif
#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <libdovi/rpu_parser.h>
#define DM_STATIC static inline
// gcc 13 reports dm_mat_inv's untaken singular-matrix path once the CM v2.9 prepare is inlined into
// this unit; the primaries matrices are constant and invertible. Scoped to the header, not this file.
#if defined(__GNUC__) && !defined(__clang__)
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wmaybe-uninitialized"
#endif
#include "../dv_dm.h"
#if defined(__GNUC__) && !defined(__clang__)
#pragma GCC diagnostic pop
#endif

static dv_dm_frame frame(DoviExtMetadataBlockLevel5 *l5) {
  DoviExtMetadataBlockLevel1 l1 = {.min_pq=12, .avg_pq=1229, .max_pq=2167};
  DoviExtMetadataBlockLevel3 l3 = {.min_pq_offset=2089, .avg_pq_offset=1863, .max_pq_offset=2048};
  DoviExtMetadataBlockLevel254 l254 = {.dm_version_index=2};
  DoviVdrDmData dm = {0};
  dm.dm_data.level1=&l1; dm.dm_data.level3=&l3;
  dm.dm_data.level254=&l254; dm.dm_data.level5=l5;
  dv_dm_frame f;
  assert(dv_dm_prepare(&dm,1000,.005,DM_P3D65,4000,true,true,&f)==NULL);
  return f;
}

// CM v2.9 (no L254, no L8). `two_planes`: a live chroma weight, so the second detail plane
// (Io) filters too; otherwise Io follows Ib. Raw ms_weight 512 keeps the detail stage on.
static dv_dm_frame frame29(DoviExtMetadataBlockLevel5 *l5, bool two_planes) {
  DoviExtMetadataBlockLevel1 l1 = {.min_pq=80, .avg_pq=1528, .max_pq=3728};
  DoviExtMetadataBlockLevel6 l6 = {.max_display_mastering_luminance=4000, .min_display_mastering_luminance=50};
  DoviExtMetadataBlockLevel2 l2 = {.target_max_pq=3079, .trim_slope=2010, .trim_offset=2180, .trim_power=2075,
      .trim_chroma_weight=two_planes?2050:2048, .trim_saturation_gain=2179, .ms_weight=512};
  const DoviExtMetadataBlockLevel2 *list[1] = {&l2};
  DoviVdrDmData dm = {0};
  dm.dm_data.level1=&l1; dm.dm_data.level6=&l6; dm.dm_data.level5=l5;
  dm.dm_data.level2.list=(__typeof__(dm.dm_data.level2.list))list; dm.dm_data.level2.len=1;
  dv_dm_frame f;
  assert(dv_dm_prepare(&dm,1000,.005,DM_P3D65,4000,true,true,&f)==NULL);
  assert(f.algo==29 && f.weight>-1 && (f.c29.tr.cw!=0)==two_planes);
  return f;
}

static void owned_offsets_and_rectangles(dv_dm_frame f, DoviExtMetadataBlockLevel5 *l5) {
  memset(l5,0,sizeof(*l5));
  dv_dm_rect r;
  assert(dv_dm_active_rect(&f,19,13,&r));
  assert(r.x0==3 && r.x1==15 && r.y0==2 && r.y1==10);
  // No borrowing of the transient parsed RPU and no offset scaling to fit.
  assert(!dv_dm_active_rect(&f,6,13,&r));
  assert(!dv_dm_active_rect(&f,19,4,&r));
  assert(!dv_dm_active_rect(&f,0,13,&r));
  assert(!dv_dm_active_rect(&f,19,-1,&r));
  const uint64_t bad[][4]={{19,0,0,0},{0,19,0,0},{3,16,0,0},
      {0,0,13,0},{0,0,0,13},{0,0,3,10},
      {UINT64_MAX,1,0,0},{1,UINT64_MAX,0,0},
      {0,0,UINT64_MAX,1},{0,0,1,UINT64_MAX}};
  for(size_t i=0;i<sizeof(bad)/sizeof(*bad);i++) {
    f.has_l5=true; f.l5_left=bad[i][0]; f.l5_right=bad[i][1];
    f.l5_top=bad[i][2]; f.l5_bottom=bad[i][3];
    assert(!dv_dm_active_rect(&f,19,13,&r));
    float in[19*13*4]={0},out[19*13*4];
    for(size_t j=0;j<sizeof(out)/sizeof(*out);j++)out[j]=-7;
    assert(!dv_dm_cpu_render(&f,in,19,13,out));
    for(size_t j=0;j<sizeof(out)/sizeof(*out);j++)assert(out[j]==-7);
  }
}

static void test_l5_owned_offsets_and_rectangles(void) {
  DoviExtMetadataBlockLevel5 l5 = {.active_area_left_offset=3,
      .active_area_right_offset=4, .active_area_top_offset=2, .active_area_bottom_offset=3};
  owned_offsets_and_rectangles(frame(&l5),&l5);
  puts("test_l5_owned_offsets_and_rectangles: PASS");
}

static void test_l5_cm29_owned_offsets_and_rectangles(void) {
  for(int planes=0;planes<2;planes++) {
    DoviExtMetadataBlockLevel5 l5 = {.active_area_left_offset=3,
        .active_area_right_offset=4, .active_area_top_offset=2, .active_area_bottom_offset=3};
    owned_offsets_and_rectangles(frame29(&l5,planes),&l5);
  }
  puts("test_l5_cm29_owned_offsets_and_rectangles: PASS");
}

static void test_l5_absent_and_zero_preserve_full_frame(void) {
  DoviExtMetadataBlockLevel5 zero={0};
  dv_dm_frame absent=frame(NULL),present=frame(&zero);
  assert(!memcmp(absent.u,present.u,sizeof(absent.u)));
  enum {W=9,H=7}; float in[W*H*4],a[W*H*4],b[W*H*4];
  for(int i=0;i<W*H;i++) {
    in[4*i]=.1f+(i%W)*.05f; in[4*i+1]=.2f+(i%H)*.02f;
    in[4*i+2]=.4f; in[4*i+3]=1;
  }
  dv_dm_rect r;
  assert(dv_dm_active_rect(&absent,W,H,&r)&&!dv_dm_active_partial(&r,W,H));
  assert(dv_dm_active_rect(&present,W,H,&r)&&!dv_dm_active_partial(&r,W,H));
  DoviExtMetadataBlockLevel5 side={.active_area_left_offset=1};
  dv_dm_frame partial=frame(&side);
  assert(dv_dm_active_rect(&partial,W,H,&r)&&dv_dm_active_partial(&r,W,H));
  assert(dv_dm_cpu_render(&absent,in,W,H,a));
  assert(dv_dm_cpu_render(&present,in,W,H,b));
  assert(!memcmp(a,b,sizeof(a)));
  puts("test_l5_absent_and_zero_preserve_full_frame: PASS");
}

// FNV-1a 64 of every full-frame shader literal as it was before any active-area body existed for
// its family (main c0a23481). A frame without a partial L5 compiles exactly these bytes.
// The common header and the two CM v2.9 full-frame bodies were re-pinned once since, when the detail
// input was held at the tone curve's top clip (c29din); the lattice and horizontal bodies are as then.
static uint64_t fnv1a(const char *s) {
  uint64_t h=UINT64_C(0xcbf29ce484222325);
  for(;*s;s++){ h^=(unsigned char)*s; h*=UINT64_C(0x100000001b3); }
  return h;
}

static void test_l5_cm29_absent_and_zero_preserve_full_frame(void) {
  const struct { const char *name,*text; size_t bytes; uint64_t hash; } pinned[]={
    {"dv_dm_glsl_common",dv_dm_glsl_common,sizeof(dv_dm_glsl_common)-1,UINT64_C(0xc3cf0592537b3c26)},
    {"dv_dm_body_point_cm29",dv_dm_body_point_cm29,sizeof(dv_dm_body_point_cm29)-1,UINT64_C(0x91b13abfbd77aa08)},
    {"dv_dm_body_lattice",dv_dm_body_lattice,sizeof(dv_dm_body_lattice)-1,UINT64_C(0x03994d4550df62ee)},
    {"dv_dm_body_h",dv_dm_body_h,sizeof(dv_dm_body_h)-1,UINT64_C(0xf0a6ba996a6b0485)},
    {"dv_dm_body_final_cm29",dv_dm_body_final_cm29,sizeof(dv_dm_body_final_cm29)-1,UINT64_C(0x5ec91d4ca829f8ec)},
    {"dv_dm_body_final_cm4",dv_dm_body_final_cm4,sizeof(dv_dm_body_final_cm4)-1,UINT64_C(0xa8af44f878d1b639)},
    {"dv_dm_body_final_active_cm4",dv_dm_body_final_active_cm4,sizeof(dv_dm_body_final_active_cm4)-1,UINT64_C(0x9748865b596d93b4)},
    {"dv_dm_body_h_active",dv_dm_body_h_active,sizeof(dv_dm_body_h_active)-1,UINT64_C(0xe407f680f7c710a2)},
  };
  for(size_t i=0;i<sizeof(pinned)/sizeof(*pinned);i++) {
    assert(strlen(pinned[i].text)==pinned[i].bytes);
    if(fnv1a(pinned[i].text)!=pinned[i].hash){ fprintf(stderr,"literal changed: %s\n",pinned[i].name); abort(); }
  }
  for(int planes=0;planes<2;planes++) {
    DoviExtMetadataBlockLevel5 zero={0},side={.active_area_bottom_offset=1};
    dv_dm_frame absent=frame29(NULL,planes),present=frame29(&zero,planes),partial=frame29(&side,planes);
    assert(!absent.has_l5 && present.has_l5 && partial.has_l5 && partial.l5_bottom==1);
    assert(!memcmp(absent.u,present.u,sizeof(absent.u)) && !memcmp(absent.u,partial.u,sizeof(absent.u)));
    enum {W=9,H=7}; float in[W*H*4],a[W*H*4],b[W*H*4],c[W*H*4];
    for(int i=0;i<W*H;i++) {
      in[4*i]=.1f+(i%W)*.05f; in[4*i+1]=.2f+(i%H)*.02f;
      in[4*i+2]=.4f; in[4*i+3]=1;
    }
    dv_dm_rect r;
    assert(dv_dm_active_rect(&absent,W,H,&r)&&!dv_dm_active_partial(&r,W,H));
    assert(dv_dm_active_rect(&present,W,H,&r)&&!dv_dm_active_partial(&r,W,H));
    assert(dv_dm_active_rect(&partial,W,H,&r)&&dv_dm_active_partial(&r,W,H)&&r.y1==H-1);
    assert(dv_dm_cpu_render(&absent,in,W,H,a));
    assert(dv_dm_cpu_render(&present,in,W,H,b));
    assert(dv_dm_cpu_render(&partial,in,W,H,c));
    assert(!memcmp(a,b,sizeof(a)));
    assert(memcmp(a,c,sizeof(a))); // a one-row bar moves the picture: the rectangle is live for CM v2.9
  }
  puts("test_l5_cm29_absent_and_zero_preserve_full_frame: PASS");
}

static void crop_domain_case(int cm,int w,int h,int left,int right,int top,int bottom,bool constant) {
  DoviExtMetadataBlockLevel5 l5={.active_area_left_offset=left,
      .active_area_right_offset=right,.active_area_top_offset=top,.active_area_bottom_offset=bottom};
  dv_dm_frame bounded=cm==4?frame(&l5):frame29(&l5,cm==292),plain=cm==4?frame(NULL):frame29(NULL,cm==292);
  assert(!memcmp(bounded.u,plain.u,sizeof(plain.u))); // L5 changes no colour/curve parameter.
  int cw=w-left-right,ch=h-top-bottom;
  size_t n=(size_t)w*h,cn=(size_t)cw*ch;
  float *a=malloc(n*16),*b=malloc(n*16),*crop=malloc(cn*16);
  float *expected=malloc(cn*16),*out_a=malloc(n*16),*out_b=malloc(n*16);
  assert(a&&b&&crop&&expected&&out_a&&out_b);
  // Two adversarial bar patterns; all active values are identical.
  for(int y=0;y<h;y++)for(int x=0;x<w;x++)for(int k=0;k<4;k++) {
    size_t i=((size_t)y*w+x)*4+k;
    a[i]=k==3?1:((x+y+k)%2?.92f:.0001f);
    b[i]=k==3?1:((x*3+y+k)%3?.01f:.73f);
  }
  for(int y=0;y<ch;y++)for(int x=0;x<cw;x++)for(int k=0;k<4;k++) {
    float v=k==3?1:constant?.35f:.12f+.025f*((x+2*y+3*k)%13);
    crop[((size_t)y*cw+x)*4+k]=v;
    a[((size_t)(y+top)*w+x+left)*4+k]=v;
    b[((size_t)(y+top)*w+x+left)*4+k]=v;
  }
  // Outside the active area the oracle is the pointwise mapping of the
  // uncropped frame: bar content is mapped, never replaced.
  dv_dm_frame point=plain; point.weight=-1;
  float *point_a=malloc(n*16),*point_b=malloc(n*16);
  assert(point_a&&point_b);
  assert(plain.weight>-1); // the detail pass is live, so the crop domain matters
  assert(dv_dm_cpu_render(&plain,crop,cw,ch,expected));
  assert(dv_dm_cpu_render(&point,a,w,h,point_a));
  assert(dv_dm_cpu_render(&point,b,w,h,point_b));
  assert(dv_dm_cpu_render(&bounded,a,w,h,out_a));
  assert(dv_dm_cpu_render(&bounded,b,w,h,out_b));
  float black=(float)dm_pq_oetf(0);
  bool bright_bar=false;
  for(int y=0;y<h;y++)for(int x=0;x<w;x++)for(int k=0;k<4;k++) {
    size_t i=((size_t)y*w+x)*4+k;
    bool inside=x>=left&&x<w-right&&y>=top&&y<h-bottom;
    float want_a=inside?expected[((size_t)(y-top)*cw+x-left)*4+k]:point_a[i];
    float want_b=inside?expected[((size_t)(y-top)*cw+x-left)*4+k]:point_b[i];
    assert(isfinite(out_a[i])&&out_a[i]==want_a&&out_b[i]==want_b);
    if(!inside&&k<3&&a[i]>.5f){ assert(out_a[i]>black+.1f); bright_bar=true; }
  }
  assert(bright_bar==(cw*ch<w*h)); // every case with bars exercises bright bar content
  free(a);free(b);free(crop);free(expected);free(out_a);free(out_b);free(point_a);free(point_b);
}

static void test_l5_crop_domain_excludes_adversarial_bars(void) {
  crop_domain_case(4,19,13,3,4,2,3,false); // all four edges, asymmetric nonconstant picture
  crop_domain_case(4,19,13,0,0,2,3,true);  // top/bottom constant picture
  crop_domain_case(4,19,13,3,4,0,0,true);  // left/right constant picture
  crop_domain_case(4,11,5,2,1,2,2,false); // height1 with a nonconstant horizontal picture
  crop_domain_case(4,7,5,3,3,2,2,true);   // width1/height1, repeated neighbors
  puts("test_l5_crop_domain_excludes_adversarial_bars: PASS");
}

// 291: one detail plane (Io follows Ib); 292: both planes filter (live chroma weight).
static void test_l5_cm29_crop_domain_excludes_adversarial_bars(void) {
  for(int cm=291;cm<=292;cm++) {
    crop_domain_case(cm,19,13,3,4,2,3,false);
    crop_domain_case(cm,19,13,0,0,2,3,true);
    crop_domain_case(cm,19,13,3,4,0,0,true);
    crop_domain_case(cm,11,5,2,1,2,2,false);
    crop_domain_case(cm,7,5,3,3,2,2,true);
    crop_domain_case(cm,23,17,0,0,1,0,false); // a single bar row above a larger picture
  }
  puts("test_l5_cm29_crop_domain_excludes_adversarial_bars: PASS");
}

// The detail stage works on the unclamped mapped intensity; only the finished value is held to the
// target range. Expected values are the licensed cm_offline output for the same picture and
// metadata: gray fields at and above the point where the mapped intensity reaches the target peak,
// one darker one-pixel speck each, which lands on min(peak, mapped field + speck - field).
//  Smile w33 (1000-nit P3 master, 600-nit target, exact L2; cm29-speck-sweep, 2026-11-12): the curve
//   passes the peak at PQ 0.693885 with slope 1, so a speck keeps its distance from that point.
//  A darkening exact L2 (gain raw 1843; 4000-nit master, L1 2/2048/3697, black 0; cm29-l2,
//   2026-11-12): the trimmed curve passes the peak at 0.9868 with slope 0.38. Holding the detail
//   input at the clip point gave 0.69629 for every speck above 0.9868 there.
static void test_cm29_detail_runs_on_the_unclamped_mapping(void) {
  static const DoviExtMetadataBlockLevel2 smile = {.target_max_pq=2851, .trim_slope=2071, .trim_offset=2048, .trim_power=1963,
      .trim_chroma_weight=2048, .trim_saturation_gain=2048, .ms_weight=2048};
  static const DoviExtMetadataBlockLevel2 dark = {.target_max_pq=2851, .trim_slope=1843, .trim_offset=2048, .trim_power=2048,
      .trim_chroma_weight=2048, .trim_saturation_gain=2048, .ms_weight=2048};
  static const struct { const DoviExtMetadataBlockLevel2 *l2; unsigned l1[3], smin, smast; double black; float field, speck; double licensed; } cases[] = {
    {&smile,{0,1229,2841},1,1000,.005, 0.72f, 0.67f, 0.67243}, {&smile,{0,1229,2841},1,1000,.005, 0.72f, 0.718f, 0.69630},
    {&smile,{0,1229,2841},1,1000,.005, 0.76f, 0.71f, 0.69630}, {&smile,{0,1229,2841},1,1000,.005, 0.90f, 0.85f, 0.69630},
    {&smile,{0,1229,2841},1,1000,.005, 0.69f, 0.64f, 0.64243},   // below the clip
    {&dark,{2,2048,3697},50,4000,0, 0.995f, 0.992f, 0.69630}, {&dark,{2,2048,3697},50,4000,0, 0.995f, 0.990f, 0.69463},
    {&dark,{2,2048,3697},50,4000,0, 0.995f, 0.987f, 0.69178}, {&dark,{2,2048,3697},50,4000,0, 0.995f, 0.975f, 0.68038},
    {&dark,{2,2048,3697},50,4000,0, 0.995f, 0.945f, 0.65183}, {&dark,{2,2048,3697},50,4000,0, 0.99f, 0.985f, 0.69275},
    {&dark,{2,2048,3697},50,4000,0, 0.99f, 0.97f, 0.67849},
  };
  enum { W=33, H=17 };
  float in[W*H*4], out[W*H*4];
  for(size_t k=0;k<sizeof(cases)/sizeof(cases[0]);k++) {
    DoviExtMetadataBlockLevel1 l1 = {.min_pq=(uint16_t)cases[k].l1[0], .avg_pq=(uint16_t)cases[k].l1[1], .max_pq=(uint16_t)cases[k].l1[2]};
    DoviExtMetadataBlockLevel6 l6 = {.max_display_mastering_luminance=(uint16_t)cases[k].smast, .min_display_mastering_luminance=(uint16_t)cases[k].smin};
    const DoviExtMetadataBlockLevel2 *list[1] = {cases[k].l2};
    DoviVdrDmData dm = {0};
    dm.dm_data.level1=&l1; dm.dm_data.level6=&l6;
    dm.dm_data.level2.list=(__typeof__(dm.dm_data.level2.list))list; dm.dm_data.level2.len=1;
    dv_dm_frame f;
    assert(dv_dm_prepare(&dm,600,cases[k].black,DM_P3D65,cases[k].smast,true,true,&f)==NULL);
    assert(f.algo==29 && f.weight>-1 && f.c29.tr.cw==0 && !strcmp(f.trim,"l2-exact"));
    for(int i=0;i<W*H;i++){ in[4*i]=in[4*i+1]=in[4*i+2]=cases[k].field; in[4*i+3]=1; }
    const int c=(H/2)*W+W/2; in[4*c]=in[4*c+1]=in[4*c+2]=cases[k].speck;
    assert(dv_dm_cpu_render(&f,in,W,H,out));
    for(int ch=0;ch<3;ch++) if(fabs(out[4*c+ch]-cases[k].licensed)>6e-5) {
      fprintf(stderr,"detail: field %.3f speck %.3f channel %d: %.6f, licensed %.5f\n",cases[k].field,cases[k].speck,ch,out[4*c+ch],cases[k].licensed); abort(); }
  }
  puts("test_cm29_detail_runs_on_the_unclamped_mapping: PASS");
}

// Large L2 chroma weights, raw 1000 and 3000 (cw -0.5117 and +0.4648), above the clip: the
// chroma-weighted plane takes its detail from the same unclamped mapping. The CPU reference must
// (a) leave a gray speck at its pointwise value even with the chroma plane live (both of its planes
// stay above the peak), and (b) give a chroma-rich speck the pinned CPU-reference value (a
// characterization of the CPU model at this commit: there is no licensed measurement of the chroma plane).
static void test_cm29_detail_chroma_weight(void) {
  static const struct { unsigned raw; double cw; float gold[3]; } cw[] = {
    {1000,(1000-2048)/2048.0,{0.6858370f,0.6660268f,0.4647649f}}, {3000,(3000-2048)/2048.0,{0.6858370f,0.6660268f,0.4647649f}},
  };
  const float speck[3]={0.97f,0.80f,0.55f};
  enum { W=33, H=17 };
  for(size_t k=0;k<sizeof(cw)/sizeof(cw[0]);k++) {
    DoviExtMetadataBlockLevel1 l1 = {.min_pq=0, .avg_pq=1229, .max_pq=2841};
    DoviExtMetadataBlockLevel6 l6 = {.max_display_mastering_luminance=1000, .min_display_mastering_luminance=1};
    DoviExtMetadataBlockLevel2 l2 = {.target_max_pq=2851, .trim_slope=2071, .trim_offset=2048, .trim_power=1963,
        .trim_chroma_weight=cw[k].raw, .trim_saturation_gain=2048, .ms_weight=2048};
    const DoviExtMetadataBlockLevel2 *list[1] = {&l2};
    DoviVdrDmData dm = {0};
    dm.dm_data.level1=&l1; dm.dm_data.level6=&l6;
    dm.dm_data.level2.list=(__typeof__(dm.dm_data.level2.list))list; dm.dm_data.level2.len=1;
    dv_dm_frame f;
    assert(dv_dm_prepare(&dm,600,.005,DM_P3D65,1000,true,true,&f)==NULL);
    assert(f.algo==29 && f.weight>-1 && fabs(f.c29.tr.cw-cw[k].cw)<1e-12 && !strcmp(f.trim,"l2-exact"));
    dm_cm29_pix px; const double rgb[3]={speck[0],speck[1],speck[2]};
    dm_cm29_pixel_pre(&f.c29,rgb,&px);
    assert(px.C>0.1 && fabs(0.5*f.c29.tr.cw*px.C)>0.01 && px.Ib>f.c29.TM && px.Io!=px.Ib);
    float in[W*H*4], out[W*H*4], uni[W*H*4];
    const int c=(H/2)*W+W/2;
    // (a) gray speck above the clip, chroma plane live: its own pointwise value
    for(int i=0;i<W*H;i++){ in[4*i]=in[4*i+1]=in[4*i+2]=0.90f; in[4*i+3]=1; }
    in[4*c]=in[4*c+1]=in[4*c+2]=0.80f;
    for(int i=0;i<W*H;i++){ uni[4*i]=uni[4*i+1]=uni[4*i+2]=0.80f; uni[4*i+3]=1; }
    assert(dv_dm_cpu_render(&f,in,W,H,out) && dv_dm_cpu_render(&f,uni,W,H,uni));
    for(int ch=0;ch<3;ch++) if(fabs(out[4*c+ch]-uni[4*c+ch])>1e-6) {
      fprintf(stderr,"raw %u gray speck: ch %d %.7f, pointwise %.7f\n",cw[k].raw,ch,out[4*c+ch],uni[4*c+ch]); abort(); }
    // (b) chroma-rich speck
    for(int i=0;i<W*H;i++){ in[4*i]=in[4*i+1]=in[4*i+2]=0.90f; in[4*i+3]=1; }
    in[4*c]=speck[0]; in[4*c+1]=speck[1]; in[4*c+2]=speck[2];
    assert(dv_dm_cpu_render(&f,in,W,H,out));
    for(int ch=0;ch<3;ch++) if(fabs(out[4*c+ch]-cw[k].gold[ch])>1e-6) {
      fprintf(stderr,"raw %u chroma speck: %.7ff,%.7ff,%.7ff pinned %.7f\n",cw[k].raw,out[4*c],out[4*c+1],out[4*c+2],cw[k].gold[ch]); abort(); }
  }
  puts("test_cm29_detail_chroma_weight: PASS");
}

// L2 trim selection and the order of trim and range clamp, against the licensed cm_offline 5.6.4
// output (cm29-l2 grid, 2026-11-12): uniform gray fields, L1 2/2048/3697 unless noted, P3 targets.
// The reference's 600- and 2000-nit targets have black 0 unless an L2 names the target.
//  interp:  L2 at 100 nits only, 4000-nit master, 600-nit target: the trim fades linearly in PQ to
//           neutral at the mastering peak, and above the L1 clip (0.9028) the darkened curve keeps
//           rising to the peak (the old clamp-then-trim order held 0.6795 there).
//  crush:   L1 205/1229/2457, exact L2, black 0.005: under the L1 crush the curve runs below the
//           target black, so black is the trim's offset alone (0.01508 lifted before).
//  smin:    L2 at 1000 only under a 0.05-nit mastering minimum: neutral at that minimum, not PQ 0.
//  atpeak:  1000-nit master, L2 at 1000 only: an L2 at the mastering peak brackets a 600-nit target
//           (untrimmed before).
//  above:   1000-nit master, exact L2 at 2000: applied (untrimmed before).
//  across:  1000-nit master, L2 at 100 and 2000: interpolated between them at 600.
//  gain:    exact darkening L2: gradation above the L1 clip (held at 0.6615 before).
static void test_cm29_l2_selection_matches_licensed(void) {
  static const DoviExtMetadataBlockLevel2 t100 = {.target_max_pq=2081, .trim_slope=1500, .trim_offset=2010, .trim_power=1300,
      .trim_chroma_weight=2048, .trim_saturation_gain=2048, .ms_weight=2048};
  static const DoviExtMetadataBlockLevel2 t600 = {.target_max_pq=2851, .trim_slope=2200, .trim_offset=2060, .trim_power=2300,
      .trim_chroma_weight=2048, .trim_saturation_gain=2048, .ms_weight=2048};
  static const DoviExtMetadataBlockLevel2 t600g = {.target_max_pq=2851, .trim_slope=1843, .trim_offset=2048, .trim_power=2048,
      .trim_chroma_weight=2048, .trim_saturation_gain=2048, .ms_weight=2048};
  static const DoviExtMetadataBlockLevel2 t1000 = {.target_max_pq=3079, .trim_slope=1400, .trim_offset=2000, .trim_power=1400,
      .trim_chroma_weight=2048, .trim_saturation_gain=2048, .ms_weight=2048};
  static const DoviExtMetadataBlockLevel2 t2000 = {.target_max_pq=3388, .trim_slope=2150, .trim_offset=2052, .trim_power=1900,
      .trim_chroma_weight=2048, .trim_saturation_gain=2048, .ms_weight=2048};
  static const struct {
    const char *name, *trim; unsigned l1[3], smin, smast; const DoviExtMetadataBlockLevel2 *l2[2]; double nits, black;
    struct { int k; double licensed; } at[5];   // gray k/2399
  } cases[] = {
    {"interp", "l2-interpolated", {2,2048,3697}, 50, 4000, {&t100}, 600, 0,
      {{720,0.27099},{1440,0.50723},{2208,0.68020},{2300,0.69492},{2376,0.69630}}},
    {"crush", "l2-exact", {205,1229,2457}, 50, 4000, {&t600}, 600, 0.005,
      {{0,0.00205},{72,0.00205},{113,0.01835},{480,0.17076},{1440,0.58305}}},
    {"smin", "l2-interpolated", {2,2048,3697}, 500, 4000, {&t1000}, 600, 0, {{720,0.25711},{1440,0.47737},{2100,0.62126}}},
    {"atpeak", "l2-interpolated", {2,2048,3697}, 50, 1000, {&t1000}, 600, 0, {{720,0.29453},{1440,0.50918},{2100,0.62419}}},
    {"above", "l2-exact", {2,2048,3697}, 50, 1000, {&t2000}, 2000, 0, {{720,0.32709},{1440,0.61833},{2100,0.82742},{2300,0.82742}}},
    {"across", "l2-interpolated", {2,2048,3697}, 50, 1000, {&t100,&t2000}, 600, 0,
      {{720,0.31530},{1440,0.55390},{2100,0.68431},{2300,0.69630}}},
    {"gain", "l2-exact", {2,2048,3697}, 50, 4000, {&t600g}, 600, 0, {{1440,0.48374},{2208,0.66925},{2300,0.68531},{2376,0.69630}}},
  };
  enum { W=33, H=17 };
  float in[W*H*4], out[W*H*4];
  for(size_t c=0;c<sizeof(cases)/sizeof(cases[0]);c++) {
    DoviExtMetadataBlockLevel1 l1 = {.min_pq=(uint16_t)cases[c].l1[0], .avg_pq=(uint16_t)cases[c].l1[1], .max_pq=(uint16_t)cases[c].l1[2]};
    DoviExtMetadataBlockLevel6 l6 = {.max_display_mastering_luminance=(uint16_t)cases[c].smast, .min_display_mastering_luminance=(uint16_t)cases[c].smin};
    DoviVdrDmData dm = {0};
    dm.dm_data.level1=&l1; dm.dm_data.level6=&l6;
    dm.dm_data.level2.list=(__typeof__(dm.dm_data.level2.list))cases[c].l2; dm.dm_data.level2.len=cases[c].l2[1]?2:1;
    dv_dm_frame f;
    assert(dv_dm_prepare(&dm,cases[c].nits,cases[c].black,DM_P3D65,cases[c].smast,true,true,&f)==NULL);
    if(f.algo!=29 || strcmp(f.trim,cases[c].trim)) { fprintf(stderr,"L2 selection %s: %s\n",cases[c].name,f.trim); abort(); }
    for(int j=0;j<5 && (j==0 || cases[c].at[j].k);j++) {
      const float v=(float)(cases[c].at[j].k/2399.0);
      for(int i=0;i<W*H;i++){ in[4*i]=in[4*i+1]=in[4*i+2]=v; in[4*i+3]=1; }
      assert(dv_dm_cpu_render(&f,in,W,H,out));
      const int m=(H/2)*W+W/2;
      for(int ch=0;ch<3;ch++) if(fabs(out[4*m+ch]-cases[c].at[j].licensed)>6e-5) {
        fprintf(stderr,"L2 selection %s: gray %.5f channel %d: %.6f, licensed %.5f\n",cases[c].name,v,ch,out[4*m+ch],cases[c].at[j].licensed); abort(); }
    }
  }
  puts("test_cm29_l2_selection_matches_licensed: PASS");
}

int main(int argc,char **argv) {
  if(argc>2)return 2;
  bool matched=argc==1;
#define RUN(name) do { if(argc==1 || !strcmp(argv[1],#name)) { name(); matched=true; } } while(0)
  RUN(test_l5_owned_offsets_and_rectangles);
  RUN(test_l5_absent_and_zero_preserve_full_frame);
  RUN(test_l5_crop_domain_excludes_adversarial_bars);
  RUN(test_l5_cm29_owned_offsets_and_rectangles);
  RUN(test_l5_cm29_absent_and_zero_preserve_full_frame);
  RUN(test_l5_cm29_crop_domain_excludes_adversarial_bars);
  RUN(test_cm29_detail_runs_on_the_unclamped_mapping);
  RUN(test_cm29_detail_chroma_weight);
  RUN(test_cm29_l2_selection_matches_licensed);
#undef RUN
  if(!matched)return 2;
  puts("test_dm_active_area: PASS");
  return 0;
}
