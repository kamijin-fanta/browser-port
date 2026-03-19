#include "SpoutGL/SpoutDX.h"
#include "SpoutGL/SpoutUtils.h"

#include <stdint.h>
#include <stdio.h>
#include <chrono>
#include <cstring>

#include <string>

extern "C" {

namespace {
thread_local std::string g_last_error;
using SteadyClock = std::chrono::steady_clock;

void set_last_error(const char* message) {
    if (message) {
        g_last_error = message;
    } else {
        g_last_error.clear();
    }
}

void set_last_errorf(
    const char* prefix,
    bool initialized,
    const char* name,
    uint32_t input_width,
    uint32_t input_height,
    unsigned int sender_width,
    unsigned int sender_height
) {
    char buf[512];
    snprintf(
        buf,
        sizeof(buf),
        "%s initialized=%d name=%s input=%ux%u sender=%ux%u",
        prefix ? prefix : "spout failure",
        initialized ? 1 : 0,
        name ? name : "(null)",
        input_width,
        input_height,
        sender_width,
        sender_height
    );
    g_last_error = buf;
}

double elapsed_ms(SteadyClock::time_point started, SteadyClock::time_point ended) {
    return std::chrono::duration<double, std::milli>(ended - started).count();
}
}  // namespace

struct BrowserPortSpoutSender {
    spoutDX* sender;
    ID3D11Texture2D* upload_texture;
    uint32_t upload_width;
    uint32_t upload_height;
    double last_swap_ms;
    double last_upload_ms;
    double last_send_ms;
    double last_total_ms;
};

struct BrowserPortSpoutReceiver {
    spoutDX* receiver;
};

void init_spout_logging() {
    static bool s_log_initialized = false;
    if (!s_log_initialized) {
        EnableSpoutLog();
        SetSpoutLogLevel(SPOUT_LOG_NOTICE);
        s_log_initialized = true;
    }
}

BrowserPortSpoutSender* browser_port_spout_create_sender_with_device(
    const char* name,
    void* device_ptr
) {
    init_spout_logging();

    BrowserPortSpoutSender* state = new BrowserPortSpoutSender();
    state->sender = new spoutDX();
    state->upload_texture = nullptr;
    state->upload_width = 0;
    state->upload_height = 0;
    state->last_swap_ms = 0.0;
    state->last_upload_ms = 0.0;
    state->last_send_ms = 0.0;
    state->last_total_ms = 0.0;
    if (!state->sender->SetSenderName(name)) {
        set_last_error("SetSenderName failed");
        delete state->sender;
        delete state;
        return nullptr;
    }
    state->sender->SetSenderFormat(DXGI_FORMAT_B8G8R8A8_UNORM);
    auto* device = reinterpret_cast<ID3D11Device*>(device_ptr);
    if (!state->sender->OpenDirectX11(device)) {
        set_last_error("OpenDirectX11 failed");
        delete state->sender;
        delete state;
        return nullptr;
    }
    set_last_error(nullptr);
    return state;
}

BrowserPortSpoutSender* browser_port_spout_create_sender(const char* name) {
    return browser_port_spout_create_sender_with_device(name, nullptr);
}

BrowserPortSpoutReceiver* browser_port_spout_create_receiver(const char* name) {
    init_spout_logging();

    BrowserPortSpoutReceiver* state = new BrowserPortSpoutReceiver();
    state->receiver = new spoutDX();
    if (name && name[0] != '\0') {
        state->receiver->SetReceiverName(name);
    }
    set_last_error(nullptr);
    return state;
}

bool ensure_upload_texture(BrowserPortSpoutSender* state, uint32_t width, uint32_t height) {
    if (!state || !state->sender || width == 0 || height == 0) {
        return false;
    }
    if (
        state->upload_texture &&
        state->upload_width == width &&
        state->upload_height == height
    ) {
        return true;
    }
    if (state->upload_texture) {
        state->upload_texture->Release();
        state->upload_texture = nullptr;
    }

    auto* device = state->sender->GetDX11Device();
    if (!device) {
        set_last_error("sender DX11 device missing");
        return false;
    }

    D3D11_TEXTURE2D_DESC desc = {};
    desc.Width = width;
    desc.Height = height;
    desc.MipLevels = 1;
    desc.ArraySize = 1;
    desc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
    desc.SampleDesc.Count = 1;
    desc.Usage = D3D11_USAGE_DEFAULT;
    desc.BindFlags = 0;
    desc.CPUAccessFlags = 0;
    desc.MiscFlags = 0;

    HRESULT hr = device->CreateTexture2D(&desc, nullptr, &state->upload_texture);
    if (FAILED(hr) || !state->upload_texture) {
        set_last_error("CreateTexture2D for BGRA upload failed");
        state->upload_texture = nullptr;
        return false;
    }

    state->upload_width = width;
    state->upload_height = height;
    return true;
}

bool browser_port_spout_send_bgra(
    BrowserPortSpoutSender* state,
    const uint8_t* bgra,
    uint32_t width,
    uint32_t height
) {
    const auto total_started = SteadyClock::now();
    if (!state || !state->sender || !bgra || width == 0 || height == 0) {
        set_last_error("invalid sender state or frame payload");
        return false;
    }
    if (!ensure_upload_texture(state, width, height)) {
        return false;
    }
    auto* context = state->sender->GetDX11Context();
    if (!context) {
        set_last_error("sender DX11 context missing");
        return false;
    }
    const auto upload_started = SteadyClock::now();
    context->UpdateSubresource(
        state->upload_texture,
        0,
        nullptr,
        bgra,
        width * 4,
        0
    );
    const auto upload_ended = SteadyClock::now();
    const auto send_started = SteadyClock::now();
    const bool sent = state->sender->SendTexture(state->upload_texture);
    const auto send_ended = SteadyClock::now();
    state->last_swap_ms = 0.0;
    state->last_upload_ms = elapsed_ms(upload_started, upload_ended);
    state->last_send_ms = elapsed_ms(send_started, send_ended);
    state->last_total_ms = elapsed_ms(total_started, send_ended);
    if (!sent) {
        set_last_errorf(
            "SendTexture(uploaded BGRA) returned false",
            state->sender->IsInitialized(),
            state->sender->GetName(),
            width,
            height,
            state->sender->GetWidth(),
            state->sender->GetHeight()
        );
        return false;
    }
    set_last_error(nullptr);
    return true;
}

bool browser_port_spout_send_dx11_texture(BrowserPortSpoutSender* state, void* texture_ptr) {
    const auto total_started = SteadyClock::now();
    if (!state || !state->sender || !texture_ptr) {
        set_last_error("invalid sender state or texture pointer");
        return false;
    }
    auto* texture = reinterpret_cast<ID3D11Texture2D*>(texture_ptr);
    D3D11_TEXTURE2D_DESC input_desc = {};
    texture->GetDesc(&input_desc);
    if (input_desc.Width == 0 || input_desc.Height == 0) {
        set_last_error("invalid input texture size");
        return false;
    }
    if (input_desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM) {
        char buf[256];
        snprintf(
            buf,
            sizeof(buf),
            "input texture format mismatch format=%u expected=%u",
            static_cast<unsigned int>(input_desc.Format),
            static_cast<unsigned int>(DXGI_FORMAT_B8G8R8A8_UNORM)
        );
        set_last_error(buf);
        return false;
    }
    auto* sender_device = state->sender->GetDX11Device();
    ID3D11Device* input_device = nullptr;
    texture->GetDevice(&input_device);
    const bool same_device = sender_device && input_device && sender_device == input_device;
    if (input_device) {
        input_device->Release();
        input_device = nullptr;
    }
    if (same_device) {
        const auto send_started = SteadyClock::now();
        const bool sent_direct = state->sender->SendTexture(texture);
        const auto send_ended = SteadyClock::now();
        if (sent_direct) {
            state->last_swap_ms = 0.0;
            state->last_upload_ms = 0.0;
            state->last_send_ms = elapsed_ms(send_started, send_ended);
            state->last_total_ms = elapsed_ms(total_started, send_ended);
            set_last_error(nullptr);
            return true;
        }
    }
    if (!ensure_upload_texture(state, input_desc.Width, input_desc.Height)) {
        return false;
    }
    auto* context = state->sender->GetDX11Context();
    if (!context) {
        set_last_error("sender DX11 context missing");
        return false;
    }
    const auto upload_started = SteadyClock::now();
    context->CopyResource(state->upload_texture, texture);
    const auto upload_ended = SteadyClock::now();
    const auto send_started = SteadyClock::now();
    const bool sent = state->sender->SendTexture(state->upload_texture);
    const auto send_ended = SteadyClock::now();
    state->last_swap_ms = 0.0;
    state->last_upload_ms = elapsed_ms(upload_started, upload_ended);
    state->last_send_ms = elapsed_ms(send_started, send_ended);
    state->last_total_ms = elapsed_ms(total_started, send_ended);
    if (!sent) {
        set_last_errorf(
            "SendTexture returned false",
            state->sender->IsInitialized(),
            state->sender->GetName(),
            input_desc.Width,
            input_desc.Height,
            state->sender->GetWidth(),
            state->sender->GetHeight()
        );
        return false;
    }
    set_last_error(nullptr);
    return true;
}

bool browser_port_spout_debug_read_sender_bgra(
    BrowserPortSpoutSender* state,
    uint8_t* bgra,
    uint32_t width,
    uint32_t height
) {
    if (!state || !state->sender || !bgra || width == 0 || height == 0) {
        set_last_error("invalid sender debug read arguments");
        return false;
    }
    auto* source = state->sender->GetSharedTexture();
    auto* device = state->sender->GetDX11Device();
    auto* context = state->sender->GetDX11Context();
    if (!source || !device || !context) {
        set_last_error("sender shared texture or device missing");
        return false;
    }

    D3D11_TEXTURE2D_DESC desc = {};
    source->GetDesc(&desc);
    if (desc.Width != width || desc.Height != height) {
        set_last_error("sender debug texture size mismatch");
        return false;
    }

    desc.Usage = D3D11_USAGE_STAGING;
    desc.BindFlags = 0;
    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    desc.MiscFlags = 0;

    ID3D11Texture2D* staging = nullptr;
    HRESULT hr = device->CreateTexture2D(&desc, nullptr, &staging);
    if (FAILED(hr) || !staging) {
        set_last_error("CreateTexture2D staging failed");
        return false;
    }

    context->CopyResource(staging, source);
    context->Flush();

    D3D11_MAPPED_SUBRESOURCE mapped = {};
    hr = context->Map(staging, 0, D3D11_MAP_READ, 0, &mapped);
    if (FAILED(hr)) {
        staging->Release();
        set_last_error("Map staging failed");
        return false;
    }

    for (uint32_t y = 0; y < height; ++y) {
        const auto* src = static_cast<const uint8_t*>(mapped.pData) + y * mapped.RowPitch;
        std::memcpy(bgra + static_cast<size_t>(y) * width * 4, src, static_cast<size_t>(width) * 4);
    }

    context->Unmap(staging, 0);
    staging->Release();
    set_last_error(nullptr);
    return true;
}

const char* browser_port_spout_last_error() {
    return g_last_error.c_str();
}

void* browser_port_spout_sender_device(BrowserPortSpoutSender* state) {
    if (!state || !state->sender) {
        return nullptr;
    }
    return state->sender->GetDX11Device();
}

bool browser_port_spout_sender_take_last_send_metrics(
    BrowserPortSpoutSender* state,
    double* swap_ms,
    double* upload_ms,
    double* send_ms,
    double* total_ms
) {
    if (!state || !swap_ms || !upload_ms || !send_ms || !total_ms) {
        set_last_error("invalid sender metrics arguments");
        return false;
    }
    *swap_ms = state->last_swap_ms;
    *upload_ms = state->last_upload_ms;
    *send_ms = state->last_send_ms;
    *total_ms = state->last_total_ms;
    set_last_error(nullptr);
    return true;
}

int browser_port_spout_sender_count() {
    init_spout_logging();
    spoutDX probe;
    return probe.GetSenderCount();
}

bool browser_port_spout_sender_name(int index, char* name, int max_len) {
    if (!name || max_len <= 0) {
        set_last_error("invalid sender name buffer");
        return false;
    }
    init_spout_logging();
    spoutDX probe;
    const bool ok = probe.GetSender(index, name, max_len);
    if (!ok) {
        set_last_error("GetSender failed");
        return false;
    }
    set_last_error(nullptr);
    return true;
}

bool browser_port_spout_sender_info(
    const char* name,
    uint32_t* width,
    uint32_t* height,
    uint64_t* share_handle,
    uint32_t* format
) {
    if (!name || !width || !height || !share_handle || !format) {
        set_last_error("invalid sender info arguments");
        return false;
    }
    init_spout_logging();
    spoutDX probe;
    unsigned int sender_width = 0;
    unsigned int sender_height = 0;
    HANDLE handle = nullptr;
    DWORD sender_format = 0;
    const bool ok = probe.GetSenderInfo(name, sender_width, sender_height, handle, sender_format);
    if (!ok) {
        set_last_error("GetSenderInfo failed");
        return false;
    }
    *width = sender_width;
    *height = sender_height;
    *share_handle = reinterpret_cast<uint64_t>(handle);
    *format = sender_format;
    set_last_error(nullptr);
    return true;
}

bool browser_port_spout_receiver_is_connected(BrowserPortSpoutReceiver* state) {
    return state && state->receiver && state->receiver->IsConnected();
}

bool browser_port_spout_receiver_is_frame_new(BrowserPortSpoutReceiver* state) {
    return state && state->receiver && state->receiver->IsFrameNew();
}

uint32_t browser_port_spout_receiver_width(BrowserPortSpoutReceiver* state) {
    if (!state || !state->receiver) {
        return 0;
    }
    return state->receiver->GetSenderWidth();
}

uint32_t browser_port_spout_receiver_height(BrowserPortSpoutReceiver* state) {
    if (!state || !state->receiver) {
        return 0;
    }
    return state->receiver->GetSenderHeight();
}

bool browser_port_spout_receiver_receive_bgra(
    BrowserPortSpoutReceiver* state,
    uint8_t* bgra,
    uint32_t width,
    uint32_t height
) {
    if (!state || !state->receiver || !bgra || width == 0 || height == 0) {
        set_last_error("invalid receiver state or frame buffer");
        return false;
    }
    if (!state->receiver->OpenDirectX11()) {
        set_last_error("receiver OpenDirectX11 failed");
        return false;
    }
    auto* device = state->receiver->GetDX11Device();
    auto* context = state->receiver->GetDX11Context();
    if (!device || !context) {
        set_last_error("receiver DX11 device/context missing");
        return false;
    }
    const bool ok = state->receiver->ReceiveTexture();
    if (!ok) {
        set_last_errorf(
            "ReceiveTexture returned false",
            state->receiver->IsInitialized(),
            state->receiver->GetSenderName(),
            width,
            height,
            state->receiver->GetSenderWidth(),
            state->receiver->GetSenderHeight()
        );
        return false;
    }
    ID3D11Texture2D* texture = state->receiver->GetSenderTexture();
    if (!texture) {
        set_last_error("ReceiveTexture returned null texture");
        return false;
    }

    D3D11_TEXTURE2D_DESC desc = {};
    texture->GetDesc(&desc);
    if (desc.Width < width || desc.Height < height) {
        set_last_error("received texture smaller than destination");
        return false;
    }
    desc.Usage = D3D11_USAGE_STAGING;
    desc.BindFlags = 0;
    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    desc.MiscFlags = 0;

    ID3D11Texture2D* staging = nullptr;
    HRESULT hr = device->CreateTexture2D(&desc, nullptr, &staging);
    if (FAILED(hr) || !staging) {
        set_last_error("CreateTexture2D staging failed");
        return false;
    }
    context->CopyResource(staging, texture);
    context->Flush();

    D3D11_MAPPED_SUBRESOURCE mapped = {};
    hr = context->Map(staging, 0, D3D11_MAP_READ, 0, &mapped);
    if (FAILED(hr)) {
        staging->Release();
        set_last_error("Map staging failed");
        return false;
    }

    for (uint32_t y = 0; y < height; ++y) {
        const auto* src = static_cast<const uint8_t*>(mapped.pData) + y * mapped.RowPitch;
        std::memcpy(bgra + static_cast<size_t>(y) * width * 4, src, static_cast<size_t>(width) * 4);
    }

    context->Unmap(staging, 0);
    staging->Release();
    set_last_error(nullptr);
    return true;
}

void browser_port_spout_destroy_sender(BrowserPortSpoutSender* state) {
    if (!state) {
        return;
    }
    if (state->sender) {
        state->sender->ReleaseSender();
        delete state->sender;
        state->sender = nullptr;
    }
    if (state->upload_texture) {
        state->upload_texture->Release();
        state->upload_texture = nullptr;
    }
    delete state;
}

void browser_port_spout_destroy_receiver(BrowserPortSpoutReceiver* state) {
    if (!state) {
        return;
    }
    if (state->receiver) {
        state->receiver->ReleaseReceiver();
        delete state->receiver;
        state->receiver = nullptr;
    }
    delete state;
}

}  // extern "C"
