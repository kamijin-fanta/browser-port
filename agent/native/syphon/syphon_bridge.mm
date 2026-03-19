#import <AppKit/AppKit.h>
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#import <QuartzCore/QuartzCore.h>
#import <Syphon/Syphon.h>

#include <stdint.h>

extern "C" {

struct BrowserPortSyphonSender {
    SyphonMetalServer* server;
    id<MTLDevice> device;
    id<MTLCommandQueue> queue;
    id<MTLTexture> texture;
    uint32_t width;
    uint32_t height;
};

BrowserPortSyphonSender* browser_port_syphon_create_sender(const char* name) {
    @autoreleasepool {
        BrowserPortSyphonSender* state = new BrowserPortSyphonSender();
        state->device = MTLCreateSystemDefaultDevice();
        if (!state->device) {
            delete state;
            return nullptr;
        }
        state->queue = [state->device newCommandQueue];
        if (!state->queue) {
            delete state;
            return nullptr;
        }
        NSString* senderName = [NSString stringWithUTF8String:name];
        state->server = [[SyphonMetalServer alloc] initWithName:senderName
                                                         device:state->device
                                                        options:nil];
        state->texture = nil;
        state->width = 0;
        state->height = 0;
        return state;
    }
}

static bool browser_port_syphon_ensure_texture(BrowserPortSyphonSender* state, uint32_t width, uint32_t height) {
    if (!state || !state->device || width == 0 || height == 0) {
        return false;
    }
    if (state->texture && state->width == width && state->height == height) {
        return true;
    }
    MTLTextureDescriptor* descriptor =
        [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatBGRA8Unorm
                                                           width:width
                                                          height:height
                                                       mipmapped:NO];
    state->texture = [state->device newTextureWithDescriptor:descriptor];
    if (!state->texture) {
        return false;
    }
    state->width = width;
    state->height = height;
    return true;
}

bool browser_port_syphon_send_bgra(
    BrowserPortSyphonSender* state,
    const uint8_t* bgra,
    uint32_t width,
    uint32_t height
) {
    @autoreleasepool {
        if (!state || !state->server || !bgra) {
            return false;
        }
        if (!browser_port_syphon_ensure_texture(state, width, height)) {
            return false;
        }
        MTLRegion region = MTLRegionMake2D(0, 0, width, height);
        const NSUInteger bytesPerRow = static_cast<NSUInteger>(width) * 4;
        [state->texture replaceRegion:region
                          mipmapLevel:0
                            withBytes:bgra
                          bytesPerRow:bytesPerRow];

        id<MTLCommandBuffer> commandBuffer = [state->queue commandBuffer];
        NSRect imageRect = NSMakeRect(0.0, 0.0, static_cast<CGFloat>(width), static_cast<CGFloat>(height));
        [state->server publishFrameTexture:state->texture
                           onCommandBuffer:commandBuffer
                               imageRegion:imageRect
                                   flipped:NO];
        [commandBuffer commit];
        [state->server publish];
        return true;
    }
}

void browser_port_syphon_destroy_sender(BrowserPortSyphonSender* state) {
    @autoreleasepool {
        if (!state) {
            return;
        }
        if (state->server) {
            [state->server stop];
            state->server = nil;
        }
        state->texture = nil;
        state->queue = nil;
        state->device = nil;
        delete state;
    }
}

}  // extern "C"

