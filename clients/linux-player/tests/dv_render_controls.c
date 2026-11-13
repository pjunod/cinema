/* Execute the actual production headers and added cleanup functions extracted
 * from the shipped patches. GPU/TA boundaries are mocked; no GPU is required. */
#include <assert.h>
#include <limits.h>
#include <math.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

enum { PL_FMT_UNORM=1, PL_FMT_UINT, PL_FMT_FLOAT, PL_FMT_CAP_LINEAR=8,
       PL_COLOR_SYSTEM_RGB=1, PL_COLOR_SYSTEM_DOLBYVISION=2,
       PL_COLOR_SYSTEM_BT_2020_NC, PL_COLOR_SYSTEM_BT_709,
       PL_COLOR_LEVELS_LIMITED=1, PL_COLOR_LEVELS_FULL=2,
       PL_COLOR_PRIM_BT_2020=9, PL_COLOR_PRIM_BT_709=1,
       PL_COLOR_TRC_PQ=16, PL_COLOR_TRC_HLG=18, PL_COLOR_TRC_BT_1886=1 };
struct pl_fmt_t { int type, num_components, component_depth[4]; unsigned caps; };
typedef const struct pl_fmt_t *pl_fmt;
typedef struct mock_gpu { struct { bool callbacks; } limits; } *pl_gpu;
struct mock_buf { uint8_t *data; };
typedef struct mock_buf *pl_buf;
typedef void *pl_tex;
struct pl_bit_encoding { int sample_depth, color_depth, bit_shift; };
struct pl_plane { bool flipped; };
struct pl_plane_data {
    int type, component_size[4], component_pad[4], component_map[4];
    size_t pixel_stride, row_stride, buf_offset;
    int width,height; bool swapped; const void *pixels; void *buf;
    void (*callback)(void *); void *priv;
};
struct pl_color_space {
    int primaries,transfer;
    struct { float min_luma,max_luma,scene_max[3],scene_avg,max_pq_y,avg_pq_y;
             float prim[8],max_cll,max_fall,ootf[19]; } hdr;
};
static const struct pl_color_space pl_color_space_hdr10={.primaries=9,.transfer=16};
static struct pl_fmt_t format={PL_FMT_UNORM,1,{16,0,0,0},PL_FMT_CAP_LINEAR};
static int allocations, frees, live, upload_calls, fail_alloc, fail_upload;
static bool no_format;
static bool expect_staged=true;
static uint8_t uploaded[4096];
static size_t uploaded_bytes;
static void *ta_alloc_size(void *parent,size_t n) {
    assert(!parent); if(fail_alloc)return NULL;
    void *p=malloc(n);assert(p);allocations++;live++;return p;
}
static void ta_free(void *p) { assert(p && live>0);free(p);frees++;live--; }
static pl_fmt pl_plane_find_fmt(pl_gpu gpu,int map[4],const struct pl_plane_data *d) {
    (void)gpu;(void)map;(void)d;return no_format?NULL:&format;
}
static bool pl_upload_plane(pl_gpu gpu,struct pl_plane *plane,pl_tex *tex,
                            const struct pl_plane_data *d) {
    (void)gpu;(void)plane;(void)tex;
    if(expect_staged) {
        assert(live==1 && d->pixels && !d->buf && !d->buf_offset);
        assert(!d->callback && !d->priv); // Never MPI callback or DR storage.
    }
    uploaded_bytes=d->row_stride*(size_t)d->height;
    assert(uploaded_bytes<=sizeof(uploaded));
    memcpy(uploaded,d->pixels,uploaded_bytes);upload_calls++;
    // Consumption can precede false, just as LP's internal PBO path does.
    return !fail_upload;
}
#include "dv_upload.h"
#include "dv_reconstruction.h"

struct mp_image;
struct slot { struct mp_image *owner; };
struct priv { struct slot hwdec,el_hwdec; };
struct vo { struct priv *priv; };
struct frame_priv { struct vo *vo; pl_tex el_tex[4]; };
struct mp_image { struct frame_priv *priv; int imgfmt,w,h; uint8_t *planes[4]; ptrdiff_t stride[4]; struct { bool no_enhancement_layer; } params; };
struct dovi { bool nlq_active; };
struct pl_frame { struct { int sys,levels; struct dovi *dovi; struct pl_bit_encoding bits; } repr;
    struct pl_color_space color; int num_planes; struct pl_plane planes[4]; };
static int destroyed, mpi_freed, slots_released, warnings;
#define MP_ARRAY_SIZE(a) ((int)(sizeof(a)/sizeof((a)[0])))
#define MP_ERR(vo,...) do { (void)(vo); warnings++; } while(0)
static void pl_tex_destroy(pl_gpu gpu,pl_tex *tex) {
    (void)gpu;assert(*tex);*tex=NULL;destroyed++;
}
static void slot_release_owner(struct priv *p,struct slot *slot,struct mp_image *owner) {
    (void)p;if(slot->owner==owner){slot->owner=NULL;slots_released++;}
}
static void talloc_free(void *ptr) {
    struct mp_image *mpi=ptr;
    for(int i=0;i<4;i++)assert(!mpi->priv->el_tex[i]);
    mpi_freed++;
}
#include "frame_cleanup.h"

static int dr_lookups,mpi_refs;
static bool force_uint;
static int mock_planes=3;
static struct pl_bit_encoding mock_bits={16,10,0};
static bool format_supported(struct vo *vo,int format_code,bool use_uint) {
    (void)vo;(void)format_code;(void)use_uint;return !force_uint;
}
static int plane_data_from_imgfmt(struct pl_plane_data d[4],struct pl_bit_encoding *bits,
                                  int imgfmt,bool use_uint) {
    (void)imgfmt;*bits=mock_bits;
    for(int p=0;p<3;p++){d[p].type=use_uint?PL_FMT_UINT:PL_FMT_UNORM;d[p].pixel_stride=2;
        d[p].component_size[0]=16;d[p].component_map[0]=p;}
    return mock_planes;
}
static int mp_image_plane_w(struct mp_image *mpi,int plane) {(void)plane;return mpi->w;}
static int mp_image_plane_h(struct mp_image *mpi,int plane) {(void)plane;return mpi->h;}
static pl_buf get_dr_buf(struct priv *p,const void *ptr) {(void)p;(void)ptr;dr_lookups++;return NULL;}
static void *mp_image_new_ref(struct mp_image *mpi) {mpi_refs++;return mpi;}
static bool pl_buf_poll(pl_gpu gpu,void *buf,uint64_t timeout) {(void)gpu;(void)buf;(void)timeout;return false;}
#include "software_upload.h"

static void test_software_upload_entry(void) {
    struct mock_gpu gpu={.limits={true}};struct priv p={0};struct vo vo={&p};
    uint16_t codes[4]={0,1,511,1023};
    struct mp_image mpi={.w=2,.h=2};
    for(int i=0;i<3;i++){mpi.planes[i]=(uint8_t*)codes;mpi.stride[i]=4;}
    struct pl_frame frame={0};pl_tex tex[4]={0};fail_upload=0;
    int before=upload_calls;
    assert(upload_planes_sw(&vo,&gpu,&mpi,&frame,tex,true));
    assert(frame.num_planes==3 && frame.repr.bits.bit_shift==6);
    assert(upload_calls==before+3 && dr_lookups==0 && mpi_refs==0 && live==0);
    fail_upload=1;before=upload_calls;
    assert(!upload_planes_sw(&vo,&gpu,&mpi,&frame,tex,true));
    assert(upload_calls==before+1 && live==0 && allocations==frees);
    fail_upload=0;expect_staged=false;
    assert(upload_planes_sw(&vo,&gpu,&mpi,&frame,tex,false));
    assert(frame.repr.bits.bit_shift==0 && dr_lookups==3 && mpi_refs==3);
    force_uint=true;
    assert(upload_planes_sw(&vo,&gpu,&mpi,&frame,tex,true));
    assert(frame.repr.bits.bit_shift==0 && dr_lookups==6 && mpi_refs==6);
    force_uint=false;expect_staged=true;
}

static void check_color_upload(struct pl_frame frame, bool expected)
{
    struct mock_gpu gpu={.limits={true}};struct priv p={0};struct vo vo={&p};
    uint16_t codes[4]={0,1,511,1023};
    struct mp_image mpi={.w=2,.h=2};pl_tex tex[4]={0};
    for(int i=0;i<3;i++){mpi.planes[i]=(uint8_t*)codes;mpi.stride[i]=4;}
    bool precise_sw10=frame_needs_precise_sw10(&frame);
    assert(precise_sw10==expected);
    expect_staged=expected;
    assert(upload_planes_sw(&vo,&gpu,&mpi,&frame,tex,precise_sw10));
    assert(frame.repr.bits.bit_shift==(expected?6:0));
    assert(codes[0]==0 && codes[1]==1 && codes[2]==511 && codes[3]==1023);
    assert(live==0 && allocations==frees);
}

static void test_color_eligibility(void)
{
    struct pl_frame hdr10={.repr={.sys=PL_COLOR_SYSTEM_BT_2020_NC,
        .levels=PL_COLOR_LEVELS_LIMITED},.color={.primaries=PL_COLOR_PRIM_BT_2020,
        .transfer=PL_COLOR_TRC_PQ}};
    check_color_upload(hdr10,true);
    struct pl_frame changed=hdr10;changed.repr.sys=PL_COLOR_SYSTEM_DOLBYVISION;
    check_color_upload(changed,true);
    changed=hdr10;changed.color.transfer=PL_COLOR_TRC_BT_1886;check_color_upload(changed,false);
    changed=hdr10;changed.color.transfer=PL_COLOR_TRC_HLG;check_color_upload(changed,false);
    changed=hdr10;changed.repr.levels=PL_COLOR_LEVELS_FULL;check_color_upload(changed,false);
    changed=hdr10;changed.repr.levels=0;check_color_upload(changed,false);
    changed=hdr10;changed.repr.sys=PL_COLOR_SYSTEM_BT_709;check_color_upload(changed,false);
    changed=hdr10;changed.color.primaries=PL_COLOR_PRIM_BT_709;check_color_upload(changed,false);
    // Eligible color cannot bypass the actual uploader's existing layout guards.
    struct mock_gpu gpu={.limits={true}};struct priv p={0};struct vo vo={&p};
    uint16_t codes[4]={0,1,511,1023};struct mp_image mpi={.w=2,.h=2};pl_tex tex[4]={0};
    for(int i=0;i<3;i++){mpi.planes[i]=(uint8_t*)codes;mpi.stride[i]=4;}
    expect_staged=false;mock_planes=2;mock_bits.bit_shift=6;
    assert(upload_planes_sw(&vo,&gpu,&mpi,&hdr10,tex,frame_needs_precise_sw10(&hdr10)));
    assert(hdr10.num_planes==2 && hdr10.repr.bits.bit_shift==6);
    mock_planes=3;mock_bits.bit_shift=0;format.caps=0;
    assert(upload_planes_sw(&vo,&gpu,&mpi,&hdr10,tex,frame_needs_precise_sw10(&hdr10)));
    assert(hdr10.repr.bits.bit_shift==0);
    format.caps=PL_FMT_CAP_LINEAR;expect_staged=true;
}

static void test_layouts(void) {
    struct pl_plane_data d[4]={0};
    for(int p=0;p<3;p++){d[p].type=PL_FMT_UNORM;d[p].pixel_stride=2;
        d[p].component_size[0]=16;d[p].component_map[0]=p;}
    struct pl_bit_encoding bits={16,10,0};
    assert(plurx_dv_shift10_supported(NULL,true,3,bits,d));
    assert(!plurx_dv_shift10_supported(NULL,false,3,bits,d));
    assert(!plurx_dv_shift10_supported(NULL,true,2,bits,d));
    bits.bit_shift=6;assert(!plurx_dv_shift10_supported(NULL,true,3,bits,d)); // P010
    bits.bit_shift=0;bits.color_depth=12;assert(!plurx_dv_shift10_supported(NULL,true,3,bits,d));bits.color_depth=10;
    d[1].type=PL_FMT_UINT;assert(!plurx_dv_shift10_supported(NULL,true,3,bits,d));d[1].type=PL_FMT_UNORM;
    d[1].swapped=true;assert(!plurx_dv_shift10_supported(NULL,true,3,bits,d));d[1].swapped=false;
    d[1].component_pad[0]=1;assert(!plurx_dv_shift10_supported(NULL,true,3,bits,d));d[1].component_pad[0]=0;
    d[1].component_size[1]=16;assert(!plurx_dv_shift10_supported(NULL,true,3,bits,d));d[1].component_size[1]=0;
    d[1].pixel_stride=4;assert(!plurx_dv_shift10_supported(NULL,true,3,bits,d));d[1].pixel_stride=2;
    format.caps=0;assert(!plurx_dv_shift10_supported(NULL,true,3,bits,d));format.caps=PL_FMT_CAP_LINEAR;
    no_format=true;assert(!plurx_dv_shift10_supported(NULL,true,3,bits,d));no_format=false;
}
static void test_pack_and_lifetime(void) {
    uint16_t codes[1024],expected[1024];uint8_t packed[2048];
    for(int i=0;i<1024;i++){codes[i]=(uint16_t)i;expected[i]=(uint16_t)(i*64);}
    assert(plurx_dv_shift10_pack(packed,sizeof(packed),(uint8_t*)codes,2048,1024,1));
    assert(!memcmp(packed,expected,sizeof(packed)));
    for(int i=0;i<1024;i++)assert(codes[i]==i);
    // Independent rational interpolation after declared 16/10/6 normalization.
    for(int i=0;i<1023;i++)for(int n=1;n<4;n++) {
        uint16_t a,b;memcpy(&a,packed+2*i,2);memcpy(&b,packed+2*(i+1),2);
        double sampled=((4-n)*(double)a+n*(double)b)/(4*65535.0);
        assert(fabs(sampled*65535.0/64.0-(i+n/4.0))<1e-10);
    }
    uint8_t rows[31];memset(rows,0xee,sizeof(rows));
    uint16_t want[9]={0,1,2,100,101,102,1021,1022,1023};
    for(int y=0;y<3;y++)for(int x=0;x<3;x++)memcpy(rows+1+10*y+2*x,&want[3*y+x],2);
    uint8_t before[31];memcpy(before,rows,sizeof(rows));
    struct pl_plane_data d={.width=3,.height=3,.pixels=(void*)1,.buf=(void*)2,.buf_offset=17,.priv=(void*)3};
    struct pl_plane plane={0};pl_tex tex=NULL;
    for(int negative=0;negative<2;negative++)for(int fail=0;fail<2;fail++) {
        fail_upload=fail;
        assert(plurx_dv_upload_shift10(NULL,&plane,&tex,d,rows+1+(negative?20:0),negative?-10:10)==!fail);
        assert(live==0 && allocations==frees && plane.flipped==!!negative);
        assert(uploaded_bytes==18 && !memcmp(rows,before,sizeof(rows)));
        for(int i=0;i<9;i++){uint16_t got;memcpy(&got,uploaded+2*i,2);assert(got==want[i]*64);}
    }
    int calls=upload_calls;
    fail_alloc=1;assert(!plurx_dv_upload_shift10(NULL,&plane,&tex,d,rows+1,10));fail_alloc=0;
    uint16_t bad=1024;memcpy(rows+1,&bad,2);
    assert(!plurx_dv_upload_shift10(NULL,&plane,&tex,d,rows+1,10));
    assert(live==0 && allocations==frees && upload_calls==calls);
    size_t row,bytes;
    assert(!plurx_dv_shift10_size(1,2,PTRDIFF_MIN,&row,&bytes));
    assert(!plurx_dv_shift10_size(INT_MAX,INT_MAX,PTRDIFF_MAX,&row,&bytes));
    assert(!plurx_dv_shift10_size(3,1,5,&row,&bytes));
    assert(!plurx_dv_shift10_size(0,1,2,&row,&bytes));
    assert(!plurx_dv_shift10_pack(packed,1,(uint8_t*)codes,2048,1024,1));
    assert(!plurx_dv_shift10_pack(packed,sizeof(packed),NULL,2048,1024,1));
}
static void test_frame_cleanup(void) {
    struct priv p={0};struct vo vo={&p};struct frame_priv fp={.vo=&vo};
    struct mp_image mpi={.priv=&fp},other={0};struct dovi dovi={true};
    struct pl_frame frame={.repr={.sys=PL_COLOR_SYSTEM_DOLBYVISION,.dovi=&dovi}};
    fp.el_tex[0]=(void*)1;fp.el_tex[2]=(void*)2;p.hwdec.owner=&mpi;p.el_hwdec.owner=&other;
    assert(!allow_missing_el(NULL,&mpi,&frame));
    assert(destroyed==2 && mpi_freed==1 && slots_released==1 && warnings==1);
    assert(!p.hwdec.owner && p.el_hwdec.owner==&other);
    // P5/P8/non-DV can have no EL and retain source/texture ownership.
    fp.el_tex[0]=(void*)1;dovi.nlq_active=false;
    assert(allow_missing_el(NULL,&mpi,&frame));
    dovi.nlq_active=true;frame.repr.sys=PL_COLOR_SYSTEM_RGB;
    assert(allow_missing_el(NULL,&mpi,&frame));
    frame.repr.sys=PL_COLOR_SYSTEM_DOLBYVISION;frame.repr.dovi=NULL;
    assert(allow_missing_el(NULL,&mpi,&frame));
    assert(mpi_freed==1 && fp.el_tex[0]);
    p.el_hwdec.owner=&mpi;free_frame_resources(NULL,&mpi);
    assert(mpi_freed==2 && destroyed==3 && slots_released==2);
}
static void test_explicit_el_suppression(void) {
    for (int is_dv=0;is_dv<2;is_dv++) for (int nlq=0;nlq<2;nlq++) {
        struct priv p={0};struct vo vo={&p};struct frame_priv fp={.vo=&vo};
        struct mp_image mpi={.priv=&fp,.params={.no_enhancement_layer=true}};
        struct dovi dovi={!!nlq};
        struct pl_frame frame={.repr={.sys=is_dv?PL_COLOR_SYSTEM_DOLBYVISION:PL_COLOR_SYSTEM_RGB,.dovi=&dovi}};
        fp.el_tex[0]=(void*)1;fp.el_tex[2]=(void*)2;
        p.hwdec.owner=&mpi;p.el_hwdec.owner=&mpi;
        const struct mp_image before=mpi;const struct pl_frame frame_before=frame;
        const struct dovi dovi_before=dovi;
        int old_freed=mpi_freed,old_destroyed=destroyed,old_slots=slots_released,old_warnings=warnings;
        assert(allow_missing_el(NULL,&mpi,&frame));
        // Explicit suppression must not rewrite RPU/NLQ state or release ownership.
        assert(!memcmp(&mpi,&before,sizeof(mpi)) && !memcmp(&frame,&frame_before,sizeof(frame)));
        assert(!memcmp(&dovi,&dovi_before,sizeof(dovi)));
        assert(mpi_freed==old_freed && destroyed==old_destroyed && slots_released==old_slots && warnings==old_warnings);
        assert(fp.el_tex[0]==(void*)1 && fp.el_tex[2]==(void*)2 && p.hwdec.owner==&mpi && p.el_hwdec.owner==&mpi);
        mpi.params.no_enhancement_layer=false;
        if (is_dv && nlq) {
            assert(!allow_missing_el(NULL,&mpi,&frame));
            assert(mpi_freed==old_freed+1 && destroyed==old_destroyed+2 && slots_released==old_slots+2 && warnings==old_warnings+1);
            assert(!fp.el_tex[0] && !fp.el_tex[2] && !p.hwdec.owner && !p.el_hwdec.owner);
        } else {
            assert(allow_missing_el(NULL,&mpi,&frame));
            assert(mpi_freed==old_freed && destroyed==old_destroyed && slots_released==old_slots && warnings==old_warnings);
            free_frame_resources(NULL,&mpi);
        }
        assert(!memcmp(&frame,&frame_before,sizeof(frame)) && !memcmp(&dovi,&dovi_before,sizeof(dovi)));
    }
}
static void test_reconstruction_color(void) {
    struct pl_color_space expected=pl_color_space_hdr10;
    expected.hdr.min_luma=.005f;expected.hdr.max_luma=10000;
    struct pl_color_space got=plurx_dv_reconstruction_color();
    assert(!memcmp(&expected,&got,sizeof(got)));
    assert(!got.hdr.scene_avg && !got.hdr.max_pq_y && !got.hdr.prim[0]);
}
int main(void) {
    test_layouts();test_pack_and_lifetime();test_frame_cleanup();test_reconstruction_color();test_software_upload_entry();test_color_eligibility();test_explicit_el_suppression();
    puts("PASS actual production packing, upload lifetime, EL cleanup and reconstruction color controls");
}
