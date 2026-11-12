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
    {"dv_dm_glsl_common",dv_dm_glsl_common,sizeof(dv_dm_glsl_common)-1,UINT64_C(0x5e81da51f2dc4c74)},
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

// Smile w33 (CM v2.9, 1000-nit P3 master, 600-nit target, exact L2): gray fields at and above the
// point where the tone curve reaches its top clip (PQ 0.693885), one darker speck each. The
// expected values are the licensed cm_offline output for the same picture and metadata
// (cm29-speck-sweep, 2026-11-12): on the clip the reference restores no detail, so each speck lands
// on its pointwise value. Before the detail input was held at the clip point the CPU reference
// gave 0.64841, 0.69446 and 0.65033 for the first three.
static void test_cm29_detail_holds_at_the_top_clip(void) {
  DoviExtMetadataBlockLevel1 l1 = {.min_pq=0, .avg_pq=1229, .max_pq=2841};
  DoviExtMetadataBlockLevel6 l6 = {.max_display_mastering_luminance=1000, .min_display_mastering_luminance=1};
  DoviExtMetadataBlockLevel2 l2 = {.target_max_pq=2851, .trim_slope=2071, .trim_offset=2048, .trim_power=1963,
      .trim_chroma_weight=2048, .trim_saturation_gain=2048, .ms_weight=2048};
  const DoviExtMetadataBlockLevel2 *list[1] = {&l2};
  DoviVdrDmData dm = {0};
  dm.dm_data.level1=&l1; dm.dm_data.level6=&l6;
  dm.dm_data.level2.list=(__typeof__(dm.dm_data.level2.list))list; dm.dm_data.level2.len=1;
  dv_dm_frame f;
  assert(dv_dm_prepare(&dm,600,.005,DM_P3D65,1000,true,true,&f)==NULL);
  assert(f.algo==29 && f.weight>-1 && f.c29.tr.cw==0 && !strcmp(f.trim,"l2-exact"));
  assert(fabs(f.c29.xtop-0.693885345)<1e-7 && f.u[10][3]==(float)f.c29.xtop);
  enum { W=33, H=17 };
  static const struct { float field, speck; double licensed; } cases[] = {
    {0.72f, 0.67f, 0.67243}, {0.72f, 0.718f, 0.69630}, {0.76f, 0.71f, 0.69630}, {0.90f, 0.85f, 0.69630},
    {0.69f, 0.64f, 0.64243},   // below the clip: detail as before
  };
  float in[W*H*4], out[W*H*4];
  for(size_t k=0;k<sizeof(cases)/sizeof(cases[0]);k++) {
    for(int i=0;i<W*H;i++){ in[4*i]=in[4*i+1]=in[4*i+2]=cases[k].field; in[4*i+3]=1; }
    const int c=(H/2)*W+W/2; in[4*c]=in[4*c+1]=in[4*c+2]=cases[k].speck;
    assert(dv_dm_cpu_render(&f,in,W,H,out));
    for(int ch=0;ch<3;ch++) if(fabs(out[4*c+ch]-cases[k].licensed)>6e-5) {
      fprintf(stderr,"top clip: field %.3f speck %.3f channel %d: %.6f, licensed %.5f\n",cases[k].field,cases[k].speck,ch,out[4*c+ch],cases[k].licensed); abort(); }
  }
  // a curve that never reaches its clip inside [0,1] leaves the detail input alone
  dm_cm29_state s=f.c29; s.TM=2.0; s.tr.lift=s.tr.gain=s.tr.gamma=0;
  const double top=dm_cm29_tc(&s,1.0); assert(top<s.TM);
  dm_cm29_pix p={.I=0.95,.C=0.1}; s.xtop=1.0; assert(dm_cm29_detail_in(&s,&p,0)==0.95);
  puts("test_cm29_detail_holds_at_the_top_clip: PASS");
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
  RUN(test_cm29_detail_holds_at_the_top_clip);
#undef RUN
  if(!matched)return 2;
  puts("test_dm_active_area: PASS");
  return 0;
}
