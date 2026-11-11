// CPU-only L5 semantic controls. No GPU/device or licensed reference required.
// The oracle is the unchanged CM4 mapper applied to a separately cropped image.
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
#include "../dv_dm.h"

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

static void test_l5_owned_offsets_and_rectangles(void) {
  DoviExtMetadataBlockLevel5 l5 = {.active_area_left_offset=3,
      .active_area_right_offset=4, .active_area_top_offset=2, .active_area_bottom_offset=3};
  dv_dm_frame f=frame(&l5); memset(&l5,0,sizeof(l5));
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
  puts("test_l5_owned_offsets_and_rectangles: PASS");
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
  assert(dv_dm_active_rect(&absent,W,H,&r)&&!dv_dm_active_cm4(&absent,&r,W,H));
  assert(dv_dm_active_rect(&present,W,H,&r)&&!dv_dm_active_cm4(&present,&r,W,H));
  DoviExtMetadataBlockLevel5 side={.active_area_left_offset=1};
  dv_dm_frame partial=frame(&side);
  assert(dv_dm_active_rect(&partial,W,H,&r)&&dv_dm_active_cm4(&partial,&r,W,H));
  partial.algo=29; // CM v2.9 never selects the active-area shaders
  assert(dv_dm_active_rect(&partial,W,H,&r)&&!dv_dm_active_cm4(&partial,&r,W,H));
  assert(dv_dm_cpu_render(&absent,in,W,H,a));
  assert(dv_dm_cpu_render(&present,in,W,H,b));
  assert(!memcmp(a,b,sizeof(a)));
  puts("test_l5_absent_and_zero_preserve_full_frame: PASS");
}

static void crop_domain_case(int w,int h,int left,int right,int top,int bottom,bool constant) {
  DoviExtMetadataBlockLevel5 l5={.active_area_left_offset=left,
      .active_area_right_offset=right,.active_area_top_offset=top,.active_area_bottom_offset=bottom};
  dv_dm_frame bounded=frame(&l5),plain=frame(NULL);
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
  crop_domain_case(19,13,3,4,2,3,false); // all four edges, asymmetric nonconstant picture
  crop_domain_case(19,13,0,0,2,3,true);  // top/bottom constant picture
  crop_domain_case(19,13,3,4,0,0,true);  // left/right constant picture
  crop_domain_case(11,5,2,1,2,2,false); // height1 with a nonconstant horizontal picture
  crop_domain_case(7,5,3,3,2,2,true);   // width1/height1, repeated neighbors
  puts("test_l5_crop_domain_excludes_adversarial_bars: PASS");
}

int main(int argc,char **argv) {
  if(argc>2)return 2;
  bool matched=argc==1;
#define RUN(name) do { if(argc==1 || !strcmp(argv[1],#name)) { name(); matched=true; } } while(0)
  RUN(test_l5_owned_offsets_and_rectangles);
  RUN(test_l5_absent_and_zero_preserve_full_frame);
  RUN(test_l5_crop_domain_excludes_adversarial_bars);
#undef RUN
  if(!matched)return 2;
  puts("test_dm_active_area: PASS");
  return 0;
}
