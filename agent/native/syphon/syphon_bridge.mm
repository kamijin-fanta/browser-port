#import <AppKit/AppKit.h>
#import <Foundation/Foundation.h>
#import <CoreVideo/CoreVideo.h>
#import <Metal/Metal.h>
#import <QuartzCore/QuartzCore.h>
#import <dispatch/dispatch.h>
#include <simd/simd.h>

#include "Syphon-Framework/SyphonServerMetalTypes.h"

#include <mach-o/dyld.h>
#include <objc/runtime.h>
#include <objc/message.h>
#include <limits.h>
#include <stdlib.h>
#include <stdint.h>
#include <stdio.h>
#include <string>

namespace {
thread_local std::string g_last_error;

void set_error(const std::string &message) {
    g_last_error = message;
}

void clear_error() {
    g_last_error.clear();
}

bool syphon_native_verbose() {
    const char *value = std::getenv("BROWSER_PORT_PERF_VERBOSE");
    if (!value || !value[0]) {
        return false;
    }
    return std::string(value) == "1" || std::string(value) == "true" ||
           std::string(value) == "TRUE" || std::string(value) == "yes" ||
           std::string(value) == "YES" || std::string(value) == "on" ||
           std::string(value) == "ON";
}

void pump_runloop_once() {
    // Syphon directory publication/lookup depends on the app's main runloop.
    // output-helper sends frames from worker threads, so pumping only the current
    // thread's runloop can leave senders undiscoverable.
    [[NSRunLoop mainRunLoop] runUntilDate:[NSDate dateWithTimeIntervalSinceNow:0.001]];
}

id call_objc_id(id target, SEL selector) {
    return ((id(*)(id, SEL))objc_msgSend)(target, selector);
}

id call_objc_id_id_id(id target, SEL selector, id arg0, id arg1) {
    return ((id(*)(id, SEL, id, id))objc_msgSend)(target, selector, arg0, arg1);
}

id call_objc_id_id_id_id(id target, SEL selector, id arg0, id arg1, id arg2) {
    return ((id(*)(id, SEL, id, id, id))objc_msgSend)(target, selector, arg0, arg1, arg2);
}

id call_objc_id_id_id_id_id(id target, SEL selector, id arg0, id arg1, id arg2, id arg3) {
    return ((id(*)(id, SEL, id, id, id, id))objc_msgSend)(target, selector, arg0, arg1, arg2, arg3);
}

BOOL call_objc_bool(id target, SEL selector) {
    return ((BOOL(*)(id, SEL))objc_msgSend)(target, selector);
}

NSUInteger call_objc_uinteger(id target, SEL selector) {
    return ((NSUInteger(*)(id, SEL))objc_msgSend)(target, selector);
}

void call_objc_void(id target, SEL selector) {
    ((void(*)(id, SEL))objc_msgSend)(target, selector);
}

void call_objc_void_bytes_region(
    id target,
    SEL selector,
    void *bytes,
    NSUInteger bytes_per_row,
    MTLRegion region,
    NSUInteger mip_level
) {
    ((void(*)(id, SEL, void *, NSUInteger, MTLRegion, NSUInteger))objc_msgSend)(
        target,
        selector,
        bytes,
        bytes_per_row,
        region,
        mip_level
    );
}

void call_objc_void_texture_publish(
    id target,
    SEL selector,
    id texture,
    id command_buffer,
    NSRect image_region,
    BOOL flipped
) {
    ((void(*)(id, SEL, id, id, NSRect, BOOL))objc_msgSend)(
        target,
        selector,
        texture,
        command_buffer,
        image_region,
        flipped
    );
}

id<MTLRenderPipelineState> make_nv12_pipeline(id<MTLDevice> device) {
    Class server_class = NSClassFromString(@"SyphonMetalServer");
    if (!server_class) {
        set_error("SyphonMetalServer class is not available");
        return nil;
    }

    NSBundle *bundle = [NSBundle bundleForClass:server_class];
    NSError *error = nil;
    id<MTLLibrary> defaultLibrary = [device newDefaultLibraryWithBundle:bundle error:&error];
    id<MTLFunction> vertexFunction = nil;
    id<MTLFunction> fragmentFunction = nil;

    if (defaultLibrary) {
        vertexFunction = [defaultLibrary newFunctionWithName:@"textureToScreenVertexShader"];
        fragmentFunction = [defaultLibrary newFunctionWithName:@"nv12ToBgraSamplingShader"];
    }

    // Fallback: compile a local NV12 shader when the bundled Syphon framework does not
    // include our added fragment entrypoint yet.
    if (!fragmentFunction) {
        static NSString *const kFallbackNV12ShaderSource =
            @"#include <metal_stdlib>\n"
             "using namespace metal;\n"
             "struct SYPHONTextureVertex { packed_float2 position; packed_float2 textureCoordinate; };\n"
             "struct RasterizerData { float4 clipSpacePosition [[position]]; float2 textureCoordinate; };\n"
             "vertex RasterizerData browserPortTextureVertexShader(uint vertexID [[vertex_id]],\n"
             "  constant SYPHONTextureVertex *vertexArray [[buffer(0)]],\n"
             "  constant uint2 *viewportSizePointer [[buffer(1)]]) {\n"
             "  RasterizerData out;\n"
             "  float2 pixelSpacePosition = float2(vertexArray[vertexID].position);\n"
             "  float2 viewportSize = float2(*viewportSizePointer);\n"
             "  out.clipSpacePosition.xy = pixelSpacePosition / (viewportSize / 2.0);\n"
             "  out.clipSpacePosition.z = 0.0;\n"
             "  out.clipSpacePosition.w = 1.0;\n"
             "  out.textureCoordinate = float2(vertexArray[vertexID].textureCoordinate);\n"
             "  return out;\n"
             "}\n"
             "fragment float4 browserPortNv12Fragment(RasterizerData in [[stage_in]],\n"
             "  texture2d<float> lumaTexture [[texture(0)]],\n"
             "  texture2d<float> chromaTexture [[texture(1)]]) {\n"
             "  constexpr sampler textureSampler (mag_filter::linear, min_filter::linear, address::clamp_to_edge);\n"
             "  float y = lumaTexture.sample(textureSampler, in.textureCoordinate).r - 0.0625;\n"
             "  float2 uv = chromaTexture.sample(textureSampler, in.textureCoordinate).rg - float2(0.5, 0.5);\n"
             "  float3 rgb = float3(\n"
             "    1.1643 * y + 1.7927 * uv.y,\n"
             "    1.1643 * y - 0.2132 * uv.x - 0.5329 * uv.y,\n"
             "    1.1643 * y + 2.1124 * uv.x);\n"
             "  return float4(clamp(rgb, 0.0, 1.0), 1.0);\n"
             "}\n";
        NSError *fallback_error = nil;
        id<MTLLibrary> fallbackLibrary = [device newLibraryWithSource:kFallbackNV12ShaderSource
                                                               options:nil
                                                                 error:&fallback_error];
        if (fallbackLibrary) {
            if (!vertexFunction) {
                vertexFunction = [fallbackLibrary newFunctionWithName:@"browserPortTextureVertexShader"];
            }
            fragmentFunction = [fallbackLibrary newFunctionWithName:@"browserPortNv12Fragment"];
        } else if (fallback_error) {
            set_error([[fallback_error localizedDescription] UTF8String]);
            return nil;
        }
    }

    if (!vertexFunction || !fragmentFunction) {
        if (error) {
            set_error([[error localizedDescription] UTF8String]);
        } else {
            set_error("NV12 shader functions are unavailable");
        }
        return nil;
    }

    MTLRenderPipelineDescriptor *descriptor = [MTLRenderPipelineDescriptor new];
    descriptor.label = @"Syphon NV12 Conversion Pipeline";
    descriptor.vertexFunction = vertexFunction;
    descriptor.fragmentFunction = fragmentFunction;
    descriptor.colorAttachments[0].pixelFormat = MTLPixelFormatBGRA8Unorm;

    id<MTLRenderPipelineState> pipeline = [device newRenderPipelineStateWithDescriptor:descriptor error:&error];
    if (!pipeline) {
        if (error) {
            set_error([[error localizedDescription] UTF8String]);
        } else {
            set_error("failed to create NV12 conversion pipeline");
        }
        return nil;
    }
    return pipeline;
}

NSArray *matching_servers(NSString *server_name) {
    pump_runloop_once();
    if (!NSClassFromString(@"SyphonServerDirectory")) {
        set_error("Syphon runtime classes are unavailable");
        return nil;
    }
    Class directory_class = NSClassFromString(@"SyphonServerDirectory");
    if (!directory_class) {
        set_error("SyphonServerDirectory class is not available");
        return nil;
    }

    id directory = call_objc_id((id)directory_class, sel_registerName("sharedDirectory"));
    if (!directory) {
        set_error("Syphon shared directory is not available");
        return nil;
    }

    SEL matching_selector = sel_registerName("serversMatchingName:appName:");
    if ([directory respondsToSelector:matching_selector]) {
        NSArray *matches = call_objc_id_id_id(
            directory,
            sel_registerName("serversMatchingName:appName:"),
            server_name,
            nil
        );
        if (matches && [matches count] > 0) {
            return matches;
        }
        // Some runtimes may not expose server names as expected; fall back to all servers.
    }

    SEL servers_selector = sel_registerName("servers");
    if (![directory respondsToSelector:servers_selector]) {
        set_error("Syphon server directory lookup API is unavailable");
        return nil;
    }

    NSArray *all_servers = call_objc_id(directory, sel_registerName("servers"));
    if (!all_servers) {
        return @[];
    }

    static NSString *const kServerNameKey = @"SyphonServerDescriptionNameKey";
    NSMutableArray *filtered = [NSMutableArray array];
    for (id entry in all_servers) {
        if (![entry isKindOfClass:[NSDictionary class]]) {
            continue;
        }
        NSDictionary *description = (NSDictionary *)entry;
        NSString *name = description[kServerNameKey];
        if ([name isKindOfClass:[NSString class]] && [name isEqualToString:server_name]) {
            [filtered addObject:description];
        }
    }
    if ([filtered count] > 0) {
        return filtered;
    }
    return all_servers;
}

NSString *resolve_executable_directory() {
    uint32_t size = PATH_MAX;
    char path[PATH_MAX];
    if (_NSGetExecutablePath(path, &size) != 0) {
        return nil;
    }
    NSString *resolved = [[NSString stringWithUTF8String:path] stringByResolvingSymlinksInPath];
    if (!resolved) {
        return nil;
    }
    return [resolved stringByDeletingLastPathComponent];
}

NSArray<NSString *> *syphon_framework_candidates() {
    NSMutableArray<NSString *> *candidates = [NSMutableArray array];

    const char *env_path = getenv("BROWSER_PORT_SYPHON_FRAMEWORK_PATH");
    if (env_path && env_path[0]) {
        NSString *explicit_path = [NSString stringWithUTF8String:env_path];
        if (explicit_path.length > 0) {
            [candidates addObject:explicit_path];
        }
    }

    NSString *exe_dir = resolve_executable_directory();
    if (exe_dir.length > 0) {
        [candidates addObject:[exe_dir stringByAppendingPathComponent:@"Frameworks/Syphon.framework"]];
        NSString *parent_frameworks = [[exe_dir stringByAppendingPathComponent:@".."] stringByAppendingPathComponent:@"Frameworks"];
        [candidates addObject:[parent_frameworks stringByAppendingPathComponent:@"Syphon.framework"]];
    }

    [candidates addObject:@"native/syphon/Syphon-Framework/build/Release/Syphon.framework"];
    return candidates;
}

bool ensure_syphon_runtime_loaded() {
    if (NSClassFromString(@"SyphonMetalServer") && NSClassFromString(@"SyphonServerDirectory")) {
        return true;
    }

    static bool attempted = false;
    if (attempted) {
        return false;
    }
    attempted = true;

    for (NSString *path in syphon_framework_candidates()) {
        NSBundle *bundle = [NSBundle bundleWithPath:path];
        if (!bundle) {
            continue;
        }
        NSError *load_error = nil;
        BOOL loaded = [bundle loadAndReturnError:&load_error] || [bundle isLoaded];
        if (loaded) {
            clear_error();
            break;
        }
        if (load_error) {
            set_error([[load_error localizedDescription] UTF8String]);
        }
    }

    if (NSClassFromString(@"SyphonMetalServer") && NSClassFromString(@"SyphonServerDirectory")) {
        return true;
    }
    if (g_last_error.empty()) {
        set_error("Syphon.framework could not be loaded (set BROWSER_PORT_SYPHON_FRAMEWORK_PATH)");
    }
    return false;
}

}  // namespace

extern "C" {

struct BrowserPortSyphonSender {
    id server;
    id<MTLDevice> device;
    id<MTLCommandQueue> queue;
    id<MTLTexture> texture;
    id<MTLRenderPipelineState> nv12Pipeline;
    CVMetalTextureCacheRef textureCache;
    uint32_t width;
    uint32_t height;
};

struct BrowserPortSyphonClient {
    NSString *server_name;
    id<MTLDevice> device;
    id client;
    uint32_t width;
    uint32_t height;
};

bool publish_texture_to_syphon(
    BrowserPortSyphonSender *state,
    id<MTLTexture> texture,
    id<MTLCommandBuffer> command_buffer
) {
    if (![NSThread isMainThread]) {
        __block bool result = false;
        dispatch_sync(dispatch_get_main_queue(), ^{
            result = publish_texture_to_syphon(state, texture, command_buffer);
        });
        if (!result && g_last_error.empty()) {
            set_error("Syphon publish failed on main queue");
        }
        return result;
    }

    if (!state || !state->server || !state->queue) {
        set_error("invalid syphon sender state");
        return false;
    }
    if (!texture || !command_buffer) {
        set_error("invalid Metal publish payload");
        return false;
    }

    SEL publish_frame_selector =
        sel_registerName("publishFrameTexture:onCommandBuffer:imageRegion:flipped:");
    if (![state->server respondsToSelector:publish_frame_selector]) {
        set_error("Syphon server publishFrameTexture API is unavailable");
        return false;
    }

    NSRect image_rect = NSMakeRect(0.0, 0.0, static_cast<CGFloat>(texture.width), static_cast<CGFloat>(texture.height));
    call_objc_void_texture_publish(
        state->server,
        publish_frame_selector,
        texture,
        command_buffer,
        image_rect,
        YES
    );

    [command_buffer commit];
    // SyphonMetalServer publishes from the command buffer completion handler.
    // Publishing early can expose an incomplete or still-cleared frame.
    pump_runloop_once();

    clear_error();
    return true;
}

const char *browser_port_syphon_last_error() {
    return g_last_error.empty() ? nullptr : g_last_error.c_str();
}

BrowserPortSyphonSender *browser_port_syphon_create_sender(const char *name) {
    if (![NSThread isMainThread]) {
        __block BrowserPortSyphonSender *result = nullptr;
        dispatch_sync(dispatch_get_main_queue(), ^{
            result = browser_port_syphon_create_sender(name);
        });
        if (!result && g_last_error.empty()) {
            set_error("failed to create syphon sender on main queue");
        }
        return result;
    }

    @autoreleasepool {
        if (!ensure_syphon_runtime_loaded()) {
            return nullptr;
        }
        if (!name || !name[0]) {
            set_error("sender name is empty");
            return nullptr;
        }

        BrowserPortSyphonSender *state = new BrowserPortSyphonSender();
        state->device = MTLCreateSystemDefaultDevice();
        if (!state->device) {
            delete state;
            set_error("failed to create Metal device");
            return nullptr;
        }

        state->queue = [state->device newCommandQueue];
        if (!state->queue) {
            delete state;
            set_error("failed to create Metal command queue");
            return nullptr;
        }

        NSString *sender_name = [NSString stringWithUTF8String:name];
        if (!sender_name) {
            delete state;
            set_error("sender name is not valid UTF-8");
            return nullptr;
        }

        Class server_class = NSClassFromString(@"SyphonMetalServer");
        if (!server_class) {
            delete state;
            set_error("SyphonMetalServer class is not available");
            return nullptr;
        }

        id server_alloc = call_objc_id((id)server_class, sel_registerName("alloc"));
        if (!server_alloc) {
            delete state;
            set_error("failed to allocate SyphonMetalServer");
            return nullptr;
        }

        SEL init_selector = sel_registerName("initWithName:device:options:");
        if (![server_alloc respondsToSelector:init_selector]) {
            delete state;
            set_error("SyphonMetalServer initializer is unavailable");
            return nullptr;
        }

        state->server = call_objc_id_id_id_id(
            server_alloc,
            init_selector,
            sender_name,
            state->device,
            nil
        );
        if (!state->server) {
            delete state;
            set_error("failed to create SyphonMetalServer instance");
            return nullptr;
        }

        if (syphon_native_verbose()) {
            fprintf(stderr, "output-helper: syphon sender native created name=%s state=%p\n", name, state);
        }

        CVReturn cache_status = CVMetalTextureCacheCreate(
            kCFAllocatorDefault,
            nullptr,
            state->device,
            nullptr,
            &state->textureCache
        );
        if (cache_status != kCVReturnSuccess || !state->textureCache) {
            delete state;
            set_error("failed to create CVMetalTextureCache");
            return nullptr;
        }

        state->nv12Pipeline = make_nv12_pipeline(state->device);
        if (!state->nv12Pipeline) {
            // Keep sender alive for BGRA publish paths even when NV12 shader isn't available.
            // This avoids complete black output when only the optional NV12 conversion path fails.
            fprintf(stderr, "browser-port syphon: NV12 pipeline unavailable (%s); using BGRA-only publish path\n", g_last_error.c_str());
            clear_error();
        }

        state->texture = nil;
        state->width = 0;
        state->height = 0;
        clear_error();
        return state;
    }
}

size_t browser_port_syphon_client_count(BrowserPortSyphonSender *state) {
    @autoreleasepool {
        if (!state || !state->server) {
            return 0;
        }
        Class base_class = NSClassFromString(@"SyphonServerBase");
        Class manager_class = NSClassFromString(@"SyphonServerConnectionManager");
        if (!base_class || !manager_class) {
            return 0;
        }

        Ivar connection_manager_ivar = class_getInstanceVariable(base_class, "_connectionManager");
        if (!connection_manager_ivar) {
            return 0;
        }
        id manager = object_getIvar(state->server, connection_manager_ivar);
        if (!manager) {
            return 0;
        }

        Ivar info_clients_ivar = class_getInstanceVariable(manager_class, "_infoClients");
        if (!info_clients_ivar) {
            return 0;
        }
        id info_clients = object_getIvar(manager, info_clients_ivar);
        if (!info_clients || ![info_clients respondsToSelector:@selector(count)]) {
            return 0;
        }
        return [info_clients count];
    }
}

static bool browser_port_syphon_ensure_texture(BrowserPortSyphonSender *state, uint32_t width, uint32_t height) {
    if (!state || !state->device || width == 0 || height == 0) {
        return false;
    }
    if (state->texture && state->width == width && state->height == height) {
        return true;
    }

    MTLTextureDescriptor *descriptor =
        [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatBGRA8Unorm
                                                           width:width
                                                          height:height
                                                       mipmapped:NO];
    descriptor.usage = MTLTextureUsageRenderTarget | MTLTextureUsageShaderRead;
    state->texture = [state->device newTextureWithDescriptor:descriptor];
    if (!state->texture) {
        return false;
    }

    state->width = width;
    state->height = height;
    return true;
}

bool browser_port_syphon_send_bgra(
    BrowserPortSyphonSender *state,
    const uint8_t *bgra,
    uint32_t width,
    uint32_t height
) {
    @autoreleasepool {
        if (!state || !state->server || !state->queue) {
            set_error("invalid syphon sender state");
            return false;
        }
        if (!bgra || width == 0 || height == 0) {
            set_error("invalid BGRA frame payload");
            return false;
        }
        if (syphon_native_verbose()) {
            fprintf(stderr, "output-helper: syphon native send bgra state=%p size=%ux%u\n", state, width, height);
        }
        if (!browser_port_syphon_ensure_texture(state, width, height)) {
            set_error("failed to allocate sender texture");
            return false;
        }

        MTLRegion region = MTLRegionMake2D(0, 0, width, height);
        const NSUInteger bytes_per_row = static_cast<NSUInteger>(width) * 4;
        [state->texture replaceRegion:region
                          mipmapLevel:0
                            withBytes:bgra
                          bytesPerRow:bytes_per_row];

        id<MTLCommandBuffer> command_buffer = [state->queue commandBuffer];
        if (!command_buffer) {
            set_error("failed to create command buffer");
            return false;
        }

        return publish_texture_to_syphon(state, state->texture, command_buffer);
    }
}

bool browser_port_syphon_send_cv_pixel_buffer(
    BrowserPortSyphonSender *state,
    CVPixelBufferRef pixel_buffer
) {
    @autoreleasepool {
        if (!state || !state->server || !state->queue || !pixel_buffer) {
            set_error("invalid syphon sender state");
            return false;
        }

        const OSType pixel_format = CVPixelBufferGetPixelFormatType(pixel_buffer);
        const uint32_t width = static_cast<uint32_t>(CVPixelBufferGetWidth(pixel_buffer));
        const uint32_t height = static_cast<uint32_t>(CVPixelBufferGetHeight(pixel_buffer));
        const size_t plane_count = CVPixelBufferGetPlaneCount(pixel_buffer);
        if (syphon_native_verbose()) {
            fprintf(
                stderr,
                "output-helper: syphon native send cv pixel buffer state=%p size=%ux%u pixel_format=0x%08x plane_count=%zu\n",
                state,
                width,
                height,
                pixel_format,
                plane_count
            );
        }
        if (width == 0 || height == 0) {
            set_error("invalid pixel buffer size");
            return false;
        }
        if (!browser_port_syphon_ensure_texture(state, width, height)) {
            set_error("failed to allocate sender texture");
            return false;
        }

        id<MTLCommandBuffer> command_buffer = [state->queue commandBuffer];
        if (!command_buffer) {
            set_error("failed to create command buffer");
            return false;
        }

        if (pixel_format == kCVPixelFormatType_32BGRA) {
            CVMetalTextureRef source_texture_ref = nullptr;
            CVReturn status = CVMetalTextureCacheCreateTextureFromImage(
                kCFAllocatorDefault,
                state->textureCache,
                pixel_buffer,
                nullptr,
                MTLPixelFormatBGRA8Unorm,
                width,
                height,
                0,
                &source_texture_ref
            );
            if (status != kCVReturnSuccess || !source_texture_ref) {
                set_error("failed to create BGRA Metal texture");
                return false;
            }
            id<MTLTexture> source_texture = CVMetalTextureGetTexture(source_texture_ref);
            bool sent = source_texture ? publish_texture_to_syphon(state, source_texture, command_buffer) : false;
            CFRelease(source_texture_ref);
            CVMetalTextureCacheFlush(state->textureCache, 0);
            return sent;
        }

        if (pixel_format != kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange &&
            pixel_format != kCVPixelFormatType_420YpCbCr8BiPlanarFullRange) {
            set_error("unsupported pixel buffer format");
            return false;
        }
        if (!state->nv12Pipeline) {
            set_error("NV12 conversion pipeline is unavailable for non-BGRA pixel buffer");
            return false;
        }

        CVMetalTextureRef y_texture_ref = nullptr;
        CVMetalTextureRef uv_texture_ref = nullptr;
        const size_t y_width = CVPixelBufferGetWidthOfPlane(pixel_buffer, 0);
        const size_t y_height = CVPixelBufferGetHeightOfPlane(pixel_buffer, 0);
        const size_t uv_width = CVPixelBufferGetWidthOfPlane(pixel_buffer, 1);
        const size_t uv_height = CVPixelBufferGetHeightOfPlane(pixel_buffer, 1);
        CVReturn status = CVMetalTextureCacheCreateTextureFromImage(
            kCFAllocatorDefault,
            state->textureCache,
            pixel_buffer,
            nullptr,
            MTLPixelFormatR8Unorm,
            y_width,
            y_height,
            0,
            &y_texture_ref
        );
        if (status != kCVReturnSuccess || !y_texture_ref) {
            set_error("failed to create NV12 luma texture");
            return false;
        }
        status = CVMetalTextureCacheCreateTextureFromImage(
            kCFAllocatorDefault,
            state->textureCache,
            pixel_buffer,
            nullptr,
            MTLPixelFormatRG8Unorm,
            uv_width,
            uv_height,
            1,
            &uv_texture_ref
        );
        if (status != kCVReturnSuccess || !uv_texture_ref) {
            CFRelease(y_texture_ref);
            set_error("failed to create NV12 chroma texture");
            return false;
        }

        id<MTLTexture> y_texture = CVMetalTextureGetTexture(y_texture_ref);
        id<MTLTexture> uv_texture = CVMetalTextureGetTexture(uv_texture_ref);
        if (!y_texture || !uv_texture) {
            CFRelease(y_texture_ref);
            CFRelease(uv_texture_ref);
            set_error("failed to resolve NV12 Metal textures");
            return false;
        }

        MTLRenderPassDescriptor *render_pass = [MTLRenderPassDescriptor renderPassDescriptor];
        render_pass.colorAttachments[0].texture = state->texture;
        render_pass.colorAttachments[0].loadAction = MTLLoadActionClear;
        render_pass.colorAttachments[0].clearColor = MTLClearColorMake(0, 0, 0, 0);
        render_pass.colorAttachments[0].storeAction = MTLStoreActionStore;

        id<MTLRenderCommandEncoder> encoder = [command_buffer renderCommandEncoderWithDescriptor:render_pass];
        if (!encoder) {
            CFRelease(y_texture_ref);
            CFRelease(uv_texture_ref);
            set_error("failed to create render encoder");
            return false;
        }

        const float w = static_cast<float>(width) / 2.0f;
        const float h = static_cast<float>(height) / 2.0f;
        const float flip_value = 1.0f;
        const SYPHONTextureVertex quadVertices[] = {
            { {  w,   flip_value * h },  { 1.f, 1.f } },
            { { -w,   flip_value * h },  { 0.f, 1.f } },
            { { -w,  flip_value * -h },  { 0.f, 0.f } },
            { {  w,  flip_value * h },   { 1.f, 1.f } },
            { { -w,  flip_value * -h },  { 0.f, 0.f } },
            { {  w,  flip_value * -h },  { 1.f, 0.f } },
        };
        const vector_uint2 viewportSize = { width, height };
        [encoder setRenderPipelineState:state->nv12Pipeline];
        [encoder setViewport:(MTLViewport){0.0, 0.0, (double)width, (double)height, -1.0, 1.0}];
        [encoder setVertexBytes:quadVertices length:sizeof(quadVertices) atIndex:SYPHONVertexInputIndexVertices];
        [encoder setVertexBytes:&viewportSize length:sizeof(viewportSize) atIndex:SYPHONVertexInputIndexViewportSize];
        [encoder setFragmentTexture:y_texture atIndex:0];
        [encoder setFragmentTexture:uv_texture atIndex:1];
        [encoder drawPrimitives:MTLPrimitiveTypeTriangle vertexStart:0 vertexCount:6];
        [encoder endEncoding];

        bool sent = publish_texture_to_syphon(state, state->texture, command_buffer);
        CFRelease(y_texture_ref);
        CFRelease(uv_texture_ref);
        CVMetalTextureCacheFlush(state->textureCache, 0);
        return sent;
    }
}

void browser_port_syphon_destroy_sender(BrowserPortSyphonSender *state) {
    @autoreleasepool {
        if (!state) {
            return;
        }
        fprintf(stderr, "output-helper: syphon sender native destroy state=%p\n", state);
        if (state->server) {
            SEL stop_selector = sel_registerName("stop");
            if ([state->server respondsToSelector:stop_selector]) {
                call_objc_void(state->server, stop_selector);
            }
            state->server = nil;
        }
        state->texture = nil;
        state->nv12Pipeline = nil;
        if (state->textureCache) {
            CFRelease(state->textureCache);
            state->textureCache = nullptr;
        }
        state->queue = nil;
        state->device = nil;
        delete state;
    }
}

static bool browser_port_syphon_connect_client(BrowserPortSyphonClient *state) {
    if (!state || state->client) {
        return state && state->client;
    }

    NSArray *servers = matching_servers(state->server_name);
    if (!servers) {
        return false;
    }
    NSDictionary *server_description = [servers firstObject];
    if (!server_description) {
        set_error("target Syphon server is not published yet");
        return false;
    }

    Class client_class = NSClassFromString(@"SyphonMetalClient");
    if (!client_class) {
        set_error("SyphonMetalClient class is not available");
        return false;
    }

    id client_alloc = call_objc_id((id)client_class, sel_registerName("alloc"));
    if (!client_alloc) {
        set_error("failed to allocate SyphonMetalClient");
        return false;
    }

    SEL init_selector = sel_registerName("initWithServerDescription:device:options:newFrameHandler:");
    if (![client_alloc respondsToSelector:init_selector]) {
        set_error("SyphonMetalClient initializer is unavailable");
        return false;
    }

    id client = call_objc_id_id_id_id_id(
        client_alloc,
        init_selector,
        server_description,
        state->device,
        nil,
        nil
    );
    if (!client) {
        set_error("failed to initialize SyphonMetalClient");
        return false;
    }

    state->client = client;
    state->width = 0;
    state->height = 0;
    clear_error();
    return true;
}

static id browser_port_syphon_client_texture(BrowserPortSyphonClient *state, bool require_new_frame) {
    if (!state) {
        set_error("client state is null");
        return nil;
    }
    if (!browser_port_syphon_connect_client(state)) {
        return nil;
    }

    if (require_new_frame) {
        SEL has_new_selector = sel_registerName("hasNewFrame");
        if ([state->client respondsToSelector:has_new_selector]
            && !call_objc_bool(state->client, has_new_selector)) {
            set_error("no new Syphon frame available");
            return nil;
        }
    }

    id frame = nil;
    SEL new_frame_selector = sel_registerName("newFrameImage");
    if ([state->client respondsToSelector:new_frame_selector]) {
        frame = call_objc_id(state->client, new_frame_selector);
    }

    id texture = nil;
    if (frame) {
        SEL texture_selector = sel_registerName("texture");
        if ([frame respondsToSelector:texture_selector]) {
            texture = call_objc_id(frame, texture_selector);
        } else {
            texture = frame;
        }
    }

    if (!texture) {
        SEL texture_selector = sel_registerName("newFrameTexture");
        if ([state->client respondsToSelector:texture_selector]) {
            texture = call_objc_id(state->client, texture_selector);
        }
    }

    if (!texture) {
        set_error("Syphon client frame texture is unavailable");
        return nil;
    }

    SEL width_selector = sel_registerName("width");
    SEL height_selector = sel_registerName("height");
    if (![texture respondsToSelector:width_selector] || ![texture respondsToSelector:height_selector]) {
        set_error("Syphon frame texture does not expose width/height");
        return nil;
    }

    state->width = static_cast<uint32_t>(call_objc_uinteger(texture, width_selector));
    state->height = static_cast<uint32_t>(call_objc_uinteger(texture, height_selector));
    clear_error();
    return texture;
}

BrowserPortSyphonClient *browser_port_syphon_create_client(const char *name) {
    @autoreleasepool {
        if (!ensure_syphon_runtime_loaded()) {
            return nullptr;
        }
        if (!name || !name[0]) {
            set_error("client server name is empty");
            return nullptr;
        }

        NSString *server_name = [NSString stringWithUTF8String:name];
        if (!server_name) {
            set_error("client server name is not valid UTF-8");
            return nullptr;
        }

        BrowserPortSyphonClient *state = new BrowserPortSyphonClient();
        state->server_name = server_name;
        state->device = MTLCreateSystemDefaultDevice();
        state->client = nil;
        state->width = 0;
        state->height = 0;

        if (!state->device) {
            delete state;
            set_error("failed to create Metal device for Syphon client");
            return nullptr;
        }

        clear_error();
        return state;
    }
}

bool browser_port_syphon_client_has_frame(BrowserPortSyphonClient *state) {
    @autoreleasepool {
        id texture = browser_port_syphon_client_texture(state, true);
        return texture != nil;
    }
}

uint32_t browser_port_syphon_client_width(BrowserPortSyphonClient *state) {
    @autoreleasepool {
        if (!state) {
            return 0;
        }
        if (state->width == 0 || state->height == 0) {
            (void)browser_port_syphon_client_texture(state, false);
        }
        return state->width;
    }
}

uint32_t browser_port_syphon_client_height(BrowserPortSyphonClient *state) {
    @autoreleasepool {
        if (!state) {
            return 0;
        }
        if (state->width == 0 || state->height == 0) {
            (void)browser_port_syphon_client_texture(state, false);
        }
        return state->height;
    }
}

bool browser_port_syphon_client_receive_bgra(
    BrowserPortSyphonClient *state,
    uint8_t *bgra,
    uint32_t width,
    uint32_t height
) {
    @autoreleasepool {
        if (!bgra || width == 0 || height == 0) {
            set_error("invalid BGRA receive buffer");
            return false;
        }

        id texture = browser_port_syphon_client_texture(state, true);
        if (!texture) {
            // Some runtimes keep hasNewFrame false while still exposing the current frame texture.
            texture = browser_port_syphon_client_texture(state, false);
            if (!texture) {
                return false;
            }
        }

        if (state->width == 0 || state->height == 0) {
            set_error("Syphon frame size is unknown");
            return false;
        }
        if (width < state->width || height < state->height) {
            set_error("receive buffer is smaller than Syphon frame size");
            return false;
        }

        SEL get_bytes_selector = sel_registerName("getBytes:bytesPerRow:fromRegion:mipmapLevel:");
        if (![texture respondsToSelector:get_bytes_selector]) {
            set_error("Syphon frame texture does not support readback");
            return false;
        }

        const NSUInteger bytes_per_row = static_cast<NSUInteger>(state->width) * 4;
        MTLRegion region = MTLRegionMake2D(0, 0, state->width, state->height);
        call_objc_void_bytes_region(texture, get_bytes_selector, bgra, bytes_per_row, region, 0);
        clear_error();
        return true;
    }
}

void browser_port_syphon_destroy_client(BrowserPortSyphonClient *state) {
    @autoreleasepool {
        if (!state) {
            return;
        }
        if (state->client) {
            SEL stop_selector = sel_registerName("stop");
            if ([state->client respondsToSelector:stop_selector]) {
                call_objc_void(state->client, stop_selector);
            }
            state->client = nil;
        }
        state->device = nil;
        state->server_name = nil;
        delete state;
    }
}

}  // extern "C"
