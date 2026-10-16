/*
 * A Photoshop-style filter used to test 8bf-host.
 *
 * The structures below are written from the published layout of the
 * Photoshop filter API; no SDK headers are needed. Compile with -DPACK4 to
 * get the Windows FilterRecord layout (#pragma pack(4)) on any platform.
 *
 * Several entry points exercise different parts of the host:
 *   InvertMain    tiled invert of color planes, parameters, globals, descriptor
 *   PlanarMain    one plane at a time, adds (plane + 1) * 10
 *   AdvanceMain   does all its work inside start using advanceState
 *   PaddingMain   shifts right by one pixel using edge-replicated input
 *   SuiteMain     uses PICA suites, the property suite and descriptors
 *   MaskMain      writes the selection mask into the color planes
 *   SampleMain    reads input at half resolution via inputRate
 *   ErrorMain     fails with errReportString
 *   CancelMain    cancels from the parameters selector
 *   ProgressMain  reports progress and stops when abortProc says so
 */

#include <stddef.h>
#include <stdint.h>
#include <string.h>

#if defined(_WIN32)
/* Windows filters always use the pack(4) FilterRecord. */
#ifndef PACK4
#define PACK4
#endif
#define EXPORT __declspec(dllexport)
#else
#define EXPORT __attribute__((visibility("default")))
#endif

typedef unsigned char Boolean;
typedef int16_t OSErr;
typedef int32_t Fixed;
typedef void **Handle;

typedef struct { int16_t v, h; } Point;
typedef struct { int16_t top, left, bottom, right; } Rect;
typedef struct { int32_t v, h; } VPoint;
typedef struct { int32_t top, left, bottom, right; } VRect;
typedef struct { uint16_t red, green, blue; } PSRGBColor;
typedef struct { Fixed v[10]; } PlugInMonitor;

typedef Boolean (*TestAbortProc)(void);
typedef void (*ProgressProc)(int32_t done, int32_t total);
typedef OSErr (*AdvanceStateProc)(void);
typedef OSErr (*GetPropertyProc)(uint32_t sig, uint32_t key, int32_t index, intptr_t *simple, Handle *complex);
typedef OSErr (*SetPropertyProc)(uint32_t sig, uint32_t key, int32_t index, intptr_t simple, Handle complex);

typedef struct {
  int16_t version, count;
  Handle (*newProc)(int32_t size);
  void (*disposeProc)(Handle h);
  int32_t (*getSizeProc)(Handle h);
  OSErr (*setSizeProc)(Handle h, int32_t size);
  char *(*lockProc)(Handle h, Boolean moveHigh);
  void (*unlockProc)(Handle h);
  void (*recoverSpaceProc)(int32_t size);
  void (*disposeRegularHandleProc)(Handle h);
} HandleProcs;

typedef struct {
  int16_t version, count;
  OSErr (*allocateProc)(int32_t size, void **id);
  char *(*lockProc)(void *id, Boolean moveHigh);
  void (*unlockProc)(void *id);
  void (*freeProc)(void *id);
  int32_t (*spaceProc)(void);
} BufferProcs;

typedef struct {
  int16_t version, count;
  GetPropertyProc getPropertyProc;
  SetPropertyProc setPropertyProc;
} PropertyProcs;

typedef void *PIWriteDescriptor;
typedef void *PIReadDescriptor;

typedef struct {
  int16_t version, count;
  PIWriteDescriptor (*openWriteDescriptorProc)(void);
  OSErr (*closeWriteDescriptorProc)(PIWriteDescriptor, Handle *);
  OSErr (*putIntegerProc)(PIWriteDescriptor, uint32_t, int32_t);
  void *rest[13];
} WriteDescriptorProcs;

typedef struct {
  int16_t version, count;
  PIReadDescriptor (*openReadDescriptorProc)(Handle, uint32_t *);
  OSErr (*closeReadDescriptorProc)(PIReadDescriptor);
  Boolean (*getKeyProc)(PIReadDescriptor, uint32_t *, uint32_t *, int32_t *);
  OSErr (*getIntegerProc)(PIReadDescriptor, int32_t *);
  void *rest[14];
} ReadDescriptorProcs;

typedef struct {
  int16_t descriptorParametersVersion;
  int16_t playInfo;
  int16_t recordInfo;
  Handle descriptor;
  WriteDescriptorProcs *writeDescriptorProcs;
  ReadDescriptorProcs *readDescriptorProcs;
} PIDescriptorParameters;

typedef struct {
  int32_t (*AcquireSuite)(const char *name, int32_t version, const void **suite);
  int32_t (*ReleaseSuite)(const char *name, int32_t version);
  Boolean (*IsEqual)(const char *a, const char *b);
  int32_t (*AllocateBlock)(size_t size, void **block);
  int32_t (*FreeBlock)(void *block);
  int32_t (*ReallocateBlock)(void *block, size_t size, void **out);
  int32_t (*Undefined)(void);
} SPBasicSuite;

typedef struct {
  Handle (*New)(int32_t size);
  void (*Dispose)(Handle h);
  void (*DisposeRegularHandle)(Handle h);
  void (*SetLock)(Handle h, Boolean lock, char **address, Boolean *oldLock);
  int32_t (*GetSize)(Handle h);
  OSErr (*SetSize)(Handle h, int32_t size);
  void (*RecoverSpace)(int32_t size);
} PSHandleSuite2;

typedef struct {
  char *(*New)(uint32_t *requested, uint32_t minimum);
  void (*Dispose)(char **buffer);
  uint32_t (*GetSize)(char *buffer);
  uint32_t (*GetSpace)(void);
} PSBufferSuite1;

typedef struct {
  void *processEvent;
  void *displayPixels;
  void *progressBar;
  void *testAbort;
  intptr_t (*MainAppWindow)(void);
} PSUIHooksSuite1;

typedef struct {
  int32_t PluginUsing32BitCoordinates;
  VPoint imageSize32;
  VRect filterRect32, inRect32, outRect32, maskRect32;
  VPoint floatCoord32, wholeSize32;
} BigDocumentStruct;

#ifdef PACK4
#pragma pack(push, 4)
#endif
typedef struct FilterRecord {
  int32_t serialNumber;
  TestAbortProc abortProc;
  ProgressProc progressProc;
  Handle parameters;
  Point imageSize;
  int16_t planes;
  Rect filterRect;
  PSRGBColor background;
  PSRGBColor foreground;
  int32_t maxSpace;
  int32_t bufferSpace;
  Rect inRect;
  int16_t inLoPlane, inHiPlane;
  Rect outRect;
  int16_t outLoPlane, outHiPlane;
  void *inData;
  int32_t inRowBytes;
  void *outData;
  int32_t outRowBytes;
  Boolean isFloating, haveMask, autoMask;
  Rect maskRect;
  void *maskData;
  int32_t maskRowBytes;
  unsigned char backColor[4], foreColor[4];
  uint32_t hostSig;
  void *hostProc;
  int16_t imageMode;
  Fixed imageHRes, imageVRes;
  Point floatCoord, wholeSize;
  PlugInMonitor monitor;
  void *platformData;
  BufferProcs *bufferProcs;
  void *resourceProcs;
  void *processEvent;
  void *displayPixels;
  HandleProcs *handleProcs;
  Boolean supportsDummyChannels, supportsAlternateLayouts;
  int16_t wantLayout, filterCase, dummyPlaneValue;
  void *premiereHook;
  AdvanceStateProc advanceState;
  Boolean supportsAbsolute, wantsAbsolute;
  GetPropertyProc getPropertyObsolete;
  Boolean cannotUndo, supportsPadding;
  int16_t inputPadding, outputPadding, maskPadding;
  char samplingSupport, reservedByte;
  Fixed inputRate, maskRate;
  void *colorServices;
  int16_t inLayerPlanes, inTransparencyMask, inLayerMasks, inInvertedLayerMasks, inNonLayerPlanes;
  int16_t outLayerPlanes, outTransparencyMask, outLayerMasks, outInvertedLayerMasks, outNonLayerPlanes;
  int16_t absLayerPlanes, absTransparencyMask, absLayerMasks, absInvertedLayerMasks, absNonLayerPlanes;
  int16_t inPreDummyPlanes, inPostDummyPlanes, outPreDummyPlanes, outPostDummyPlanes;
  int32_t inColumnBytes, inPlaneBytes, outColumnBytes, outPlaneBytes;
  void *imageServicesProcs;
  PropertyProcs *propertyProcs;
  int16_t inTileHeight, inTileWidth;
  Point inTileOrigin;
  int16_t absTileHeight, absTileWidth;
  Point absTileOrigin;
  int16_t outTileHeight, outTileWidth;
  Point outTileOrigin;
  int16_t maskTileHeight, maskTileWidth;
  Point maskTileOrigin;
  PIDescriptorParameters *descriptorParameters;
  unsigned char *errorString;
  void *channelPortProcs;
  void *documentInfo;
  SPBasicSuite *sSPBasic;
  void *plugInRef;
  int32_t depth;
  Handle iCCprofileData;
  int32_t iCCprofileSize;
  int32_t canUseICCProfiles;
  int32_t hasImageScrap;
  BigDocumentStruct *bigDocumentData;
  void *input3DScene;
  void *output3DScene;
  Boolean createNewLayer;
  Handle iCCWorkingProfileData;
  int32_t iCCWorkingProfileSize;
  int64_t bufferSpace64;
  int64_t maxSpace64;
  Boolean allowOverrangeColors;
  char reserved[45];
} FilterRecord;
#ifdef PACK4
#pragma pack(pop)
#endif

enum {
  selAbout = 0, selParameters, selPrepare, selStart, selContinue, selFinish
};

#define noErr 0
#define userCanceledErr (-128)
#define paramErr (-50)
#define errReportString (-30904)
#define errHostInsufficient (-30900)
#define FOURCC(a, b, c, d) (((uint32_t)(a) << 24) | ((uint32_t)(b) << 16) | ((uint32_t)(c) << 8) | (uint32_t)(d))
#define TILE_ROWS 7

/* Exported so the tests can check struct layout from both sides. */
EXPORT int32_t TestFilterRecordSize(void) { return (int32_t)sizeof(FilterRecord); }
EXPORT int32_t TestFilterRecordOffsetOfDepth(void) { return (int32_t)offsetof(FilterRecord, depth); }

typedef struct {
  int32_t magic;
  int32_t amount;
} Params;

typedef struct {
  int32_t runs;
  int32_t row;
  int32_t plane;
} Globals;

static int color_planes(const FilterRecord *r) {
  int n = r->inLayerPlanes + r->inNonLayerPlanes;
  return n > 0 ? n : r->planes;
}

static int bytes_per_sample(const FilterRecord *r) { return r->depth == 16 ? 2 : 1; }

static Globals *globals(FilterRecord *r, intptr_t *data) {
  if (*data == 0) {
    Handle h = r->handleProcs->newProc(sizeof(Globals));
    if (!h) return 0;
    *data = (intptr_t)h;
  }
  /* Use the handle the classic way: dereference the master pointer. */
  return (Globals *)*(Handle)*data;
}

static void set_tile(FilterRecord *r, int row, int lo, int hi) {
  int bottom = row + TILE_ROWS;
  if (bottom > r->filterRect.bottom) bottom = r->filterRect.bottom;
  r->inRect = r->filterRect;
  r->inRect.top = (int16_t)row;
  r->inRect.bottom = (int16_t)bottom;
  r->outRect = r->inRect;
  r->inLoPlane = r->outLoPlane = (int16_t)lo;
  r->inHiPlane = r->outHiPlane = (int16_t)hi;
}

static void clear_rects(FilterRecord *r) {
  memset(&r->inRect, 0, sizeof(Rect));
  memset(&r->outRect, 0, sizeof(Rect));
  memset(&r->maskRect, 0, sizeof(Rect));
}

static void report(FilterRecord *r, const char *message, int16_t *result) {
  size_t n = strlen(message);
  if (n > 255) n = 255;
  r->errorString[0] = (unsigned char)n;
  memcpy(r->errorString + 1, message, n);
  *result = errReportString;
}

static void invert_tile(FilterRecord *r) {
  int planes = r->inHiPlane - r->inLoPlane + 1;
  int color = color_planes(r);
  int bps = bytes_per_sample(r);
  int width = r->outRect.right - r->outRect.left;
  int height = r->outRect.bottom - r->outRect.top;
  for (int y = 0; y < height; y++) {
    unsigned char *in = (unsigned char *)r->inData + y * r->inRowBytes;
    unsigned char *out = (unsigned char *)r->outData + y * r->outRowBytes;
    for (int x = 0; x < width; x++) {
      for (int p = 0; p < planes; p++) {
        int plane = r->inLoPlane + p;
        int i = (x * planes + p);
        if (bps == 1) {
          out[i] = plane < color ? (unsigned char)(255 - in[i]) : in[i];
        } else {
          uint16_t v = ((uint16_t *)in)[i];
          ((uint16_t *)out)[i] = plane < color ? (uint16_t)(32768 - v) : v;
        }
      }
    }
  }
}

static Params *params(FilterRecord *r) {
  if (!r->parameters) return 0;
  return (Params *)r->handleProcs->lockProc(r->parameters, 0);
}

static void write_descriptor(FilterRecord *r, int32_t amount, int32_t runs) {
  PIDescriptorParameters *d = r->descriptorParameters;
  if (!d || !d->writeDescriptorProcs) return;
  PIWriteDescriptor w = d->writeDescriptorProcs->openWriteDescriptorProc();
  d->writeDescriptorProcs->putIntegerProc(w, FOURCC('A', 'm', 'n', 't'), amount);
  d->writeDescriptorProcs->putIntegerProc(w, FOURCC('R', 'u', 'n', 's'), runs);
  if (d->descriptor) r->handleProcs->disposeProc(d->descriptor);
  d->writeDescriptorProcs->closeWriteDescriptorProc(w, &d->descriptor);
}

EXPORT void InvertMain(int16_t selector, FilterRecord *r, intptr_t *data, int16_t *result) {
  *result = noErr;
  Globals *g;
  Params *p;
  switch (selector) {
  case selParameters:
    if (!r->parameters) {
      r->parameters = r->handleProcs->newProc(sizeof(Params));
      p = params(r);
      p->magic = 0x1234;
      p->amount = 5;
      r->handleProcs->unlockProc(r->parameters);
    }
    break;
  case selPrepare:
    r->bufferSpace = 0;
    break;
  case selStart:
    p = params(r);
    if (!p || p->magic != 0x1234) {
      report(r, "parameters were lost", result);
      return;
    }
    g = globals(r, data);
    g->runs++;
    g->row = r->filterRect.top;
    set_tile(r, g->row, 0, r->planes - 1);
    break;
  case selContinue:
    g = globals(r, data);
    invert_tile(r);
    g->row += TILE_ROWS;
    if (g->row >= r->filterRect.bottom)
      clear_rects(r);
    else
      set_tile(r, g->row, 0, r->planes - 1);
    break;
  case selFinish:
    g = globals(r, data);
    p = params(r);
    write_descriptor(r, p ? p->amount : -1, g ? g->runs : -1);
    break;
  }
}

EXPORT void PlanarMain(int16_t selector, FilterRecord *r, intptr_t *data, int16_t *result) {
  *result = noErr;
  Globals *g = (selector >= selStart) ? globals(r, data) : 0;
  switch (selector) {
  case selStart:
    g->plane = 0;
    g->row = r->filterRect.top;
    set_tile(r, g->row, 0, 0);
    break;
  case selContinue: {
    int bps = bytes_per_sample(r);
    int width = r->outRect.right - r->outRect.left;
    int height = r->outRect.bottom - r->outRect.top;
    int add = (r->outLoPlane + 1) * 10;
    /* Step by the column strides, as real filters do: with one plane
       requested they must be one sample, not one pixel of every plane. */
    for (int y = 0; y < height; y++) {
      unsigned char *in_row = (unsigned char *)r->inData + y * r->inRowBytes;
      unsigned char *out_row = (unsigned char *)r->outData + y * r->outRowBytes;
      for (int x = 0; x < width; x++) {
        unsigned char *in = in_row + x * r->inColumnBytes;
        unsigned char *out = out_row + x * r->outColumnBytes;
        if (bps == 1) {
          int v = in[0] + add;
          out[0] = (unsigned char)(v > 255 ? 255 : v);
        } else {
          int v = *(uint16_t *)in + add * 128;
          *(uint16_t *)out = (uint16_t)(v > 32768 ? 32768 : v);
        }
      }
    }
    g->row += TILE_ROWS;
    if (g->row >= r->filterRect.bottom) {
      g->row = r->filterRect.top;
      g->plane++;
    }
    if (g->plane >= color_planes(r))
      clear_rects(r);
    else
      set_tile(r, g->row, g->plane, g->plane);
    break;
  }
  }
}

EXPORT void AdvanceMain(int16_t selector, FilterRecord *r, intptr_t *data, int16_t *result) {
  (void)data;
  *result = noErr;
  if (selector != selStart) return;
  for (int row = r->filterRect.top; row < r->filterRect.bottom; row += TILE_ROWS) {
    set_tile(r, row, 0, r->planes - 1);
    OSErr err = r->advanceState();
    if (err) {
      *result = err;
      return;
    }
    invert_tile(r);
  }
  clear_rects(r);
}

EXPORT void PaddingMain(int16_t selector, FilterRecord *r, intptr_t *data, int16_t *result) {
  (void)data;
  *result = noErr;
  if (selector != selStart) return;
  int planes = r->planes;
  int bps = bytes_per_sample(r);
  r->inputPadding = -1; /* plugInWantsEdgeReplication */
  r->outRect = r->filterRect;
  r->inRect = r->filterRect;
  r->inRect.left -= 1;
  r->inRect.right -= 1;
  r->inLoPlane = r->outLoPlane = 0;
  r->inHiPlane = r->outHiPlane = (int16_t)(planes - 1);
  OSErr err = r->advanceState();
  if (err) {
    *result = err;
    return;
  }
  int width = r->outRect.right - r->outRect.left;
  int height = r->outRect.bottom - r->outRect.top;
  for (int y = 0; y < height; y++)
    memcpy((char *)r->outData + y * r->outRowBytes, (char *)r->inData + y * r->inRowBytes,
           (size_t)(width * planes * bps));
  clear_rects(r);
}

static uint32_t read_amount(FilterRecord *r, int32_t fallback) {
  PIDescriptorParameters *d = r->descriptorParameters;
  int32_t amount = fallback;
  if (!d || !d->descriptor || !d->readDescriptorProcs) return (uint32_t)amount;
  uint32_t keys[] = {FOURCC('A', 'm', 'n', 't'), 0};
  PIReadDescriptor rd = d->readDescriptorProcs->openReadDescriptorProc(d->descriptor, keys);
  uint32_t key, type;
  int32_t flags;
  while (d->readDescriptorProcs->getKeyProc(rd, &key, &type, &flags)) {
    if (key == FOURCC('A', 'm', 'n', 't')) d->readDescriptorProcs->getIntegerProc(rd, &amount);
  }
  d->readDescriptorProcs->closeReadDescriptorProc(rd);
  return (uint32_t)amount;
}

EXPORT void SuiteMain(int16_t selector, FilterRecord *r, intptr_t *data, int16_t *result) {
  (void)data;
  *result = noErr;
  if (selector != selStart) return;
  SPBasicSuite *sp = r->sSPBasic;
  const PSHandleSuite2 *hs = 0;
  const PSBufferSuite1 *bs = 0;
  const PSUIHooksSuite1 *ui = 0;
  const void *missing = (const void *)1;
  if (sp->AcquireSuite("Photoshop Handle Suite for Plug-ins", 2, (const void **)&hs) || !hs) {
    report(r, "no handle suite", result);
    return;
  }
  if (sp->AcquireSuite("Photoshop Buffer Suite for Plug-ins", 1, (const void **)&bs) || !bs) {
    report(r, "no buffer suite", result);
    return;
  }
  if (sp->AcquireSuite("Photoshop UIHooks Suite for Plug-ins", 1, (const void **)&ui) || !ui) {
    report(r, "no UI hooks suite", result);
    return;
  }
  if (sp->AcquireSuite("No Such Suite", 1, &missing) == 0) {
    report(r, "acquired a suite that does not exist", result);
    return;
  }
  Handle h = hs->New(4);
  char *address = 0;
  Boolean old = 0;
  hs->SetLock(h, 1, &address, &old);
  memcpy(address, "abc", 4);
  hs->SetLock(h, 0, &address, &old);
  if (hs->SetSize(h, 4096) || hs->GetSize(h) != 4096 || strcmp((char *)*h, "abc") != 0) {
    report(r, "handle resize lost data", result);
    return;
  }
  hs->Dispose(h);
  uint32_t size = 1000;
  char *buffer = bs->New(&size, 10);
  if (!buffer || size != 1000 || bs->GetSize(buffer) != 1000) {
    report(r, "buffer suite failed", result);
    return;
  }
  bs->Dispose(&buffer);
  if (buffer) {
    report(r, "buffer dispose did not clear the pointer", result);
    return;
  }
  void *block = 0;
  if (sp->AllocateBlock(16, &block) || sp->ReallocateBlock(block, 64, &block) || sp->FreeBlock(block)) {
    report(r, "block allocation failed", result);
    return;
  }
  intptr_t channels = 0;
  if (r->propertyProcs->getPropertyProc(FOURCC('8', 'B', 'I', 'M'), FOURCC('n', 'u', 'c', 'h'), 0,
                                        &channels, 0) ||
      channels != r->planes) {
    report(r, "property suite returned the wrong channel count", result);
    return;
  }
  Handle title = 0;
  if (r->propertyProcs->getPropertyProc(FOURCC('8', 'B', 'I', 'M'), FOURCC('t', 'i', 't', 'l'), 0, 0,
                                        &title) ||
      !title) {
    report(r, "no title property", result);
    return;
  }
  r->handleProcs->disposeProc(title);
  intptr_t parent = ui->MainAppWindow();

  uint32_t value = read_amount(r, 7) + (uint32_t)(parent & 0xF);
  r->outRect = r->filterRect;
  r->outLoPlane = 0;
  r->outHiPlane = (int16_t)(color_planes(r) - 1);
  OSErr err = r->advanceState();
  if (err) {
    *result = err;
    return;
  }
  int planes = r->outHiPlane - r->outLoPlane + 1;
  int bps = bytes_per_sample(r);
  int width = r->outRect.right - r->outRect.left;
  int height = r->outRect.bottom - r->outRect.top;
  for (int y = 0; y < height; y++) {
    unsigned char *out = (unsigned char *)r->outData + y * r->outRowBytes;
    for (int i = 0; i < width * planes; i++) {
      if (bps == 1)
        out[i] = (unsigned char)value;
      else
        ((uint16_t *)out)[i] = (uint16_t)value;
    }
  }
  clear_rects(r);
  write_descriptor(r, (int32_t)value, 0);
}

EXPORT void MaskMain(int16_t selector, FilterRecord *r, intptr_t *data, int16_t *result) {
  (void)data;
  *result = noErr;
  if (selector != selStart) return;
  if (!r->haveMask) {
    report(r, "expected a selection mask", result);
    return;
  }
  r->autoMask = 0;
  r->maskRect = r->filterRect;
  r->outRect = r->filterRect;
  r->outLoPlane = 0;
  r->outHiPlane = 0;
  OSErr err = r->advanceState();
  if (err) {
    *result = err;
    return;
  }
  int width = r->outRect.right - r->outRect.left;
  int height = r->outRect.bottom - r->outRect.top;
  for (int y = 0; y < height; y++)
    memcpy((char *)r->outData + y * r->outRowBytes, (char *)r->maskData + y * r->maskRowBytes,
           (size_t)width);
  clear_rects(r);
}

EXPORT void SampleMain(int16_t selector, FilterRecord *r, intptr_t *data, int16_t *result) {
  (void)data;
  *result = noErr;
  if (selector != selStart) return;
  /* Read the whole image at half size, write each sample to a 2x2 block. */
  r->inputRate = 2 << 16;
  r->inRect.top = (int16_t)(r->filterRect.top / 2);
  r->inRect.left = (int16_t)(r->filterRect.left / 2);
  r->inRect.bottom = (int16_t)(r->filterRect.bottom / 2);
  r->inRect.right = (int16_t)(r->filterRect.right / 2);
  r->inLoPlane = 0;
  r->inHiPlane = 0;
  r->outRect = r->filterRect;
  r->outLoPlane = 0;
  r->outHiPlane = 0;
  OSErr err = r->advanceState();
  if (err) {
    *result = err;
    return;
  }
  int width = r->outRect.right - r->outRect.left;
  int height = r->outRect.bottom - r->outRect.top;
  for (int y = 0; y < height; y++) {
    unsigned char *out = (unsigned char *)r->outData + y * r->outRowBytes;
    unsigned char *in = (unsigned char *)r->inData + (y / 2) * r->inRowBytes;
    for (int x = 0; x < width; x++) out[x] = in[x / 2];
  }
  clear_rects(r);
}

EXPORT void ErrorMain(int16_t selector, FilterRecord *r, intptr_t *data, int16_t *result) {
  (void)data;
  *result = noErr;
  if (selector == selStart) report(r, "the test filter failed on purpose", result);
}

EXPORT void CancelMain(int16_t selector, FilterRecord *r, intptr_t *data, int16_t *result) {
  (void)r;
  (void)data;
  *result = selector == selParameters ? userCanceledErr : noErr;
}

EXPORT void ProgressMain(int16_t selector, FilterRecord *r, intptr_t *data, int16_t *result) {
  (void)data;
  *result = noErr;
  if (selector != selStart) return;
  for (int i = 0; i < 100; i++) {
    r->progressProc(i, 100);
    if (r->abortProc()) {
      *result = userCanceledErr;
      return;
    }
  }
}
