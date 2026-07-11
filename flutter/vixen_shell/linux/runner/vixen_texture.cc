#include "vixen_texture.h"

#include <cstring>

namespace {

constexpr size_t kPoolSize = 3;
constexpr uint32_t kMaxDimension = 4096;
constexpr size_t kMaxFrameBytes = 64U * 1024U * 1024U;

GQuark vixen_texture_error_quark() {
  return g_quark_from_static_string("vixen-texture-error");
}

}  // namespace

struct _VixenTexture {
  FlPixelBufferTexture parent_instance;
  GMutex mutex;
  uint8_t* buffers[kPoolSize];
  size_t capacities[kPoolSize];
  size_t lengths[kPoolSize];
  uint32_t widths[kPoolSize];
  uint32_t heights[kPoolSize];
  int current_index;
  int returned_index;
};

G_DEFINE_TYPE(VixenTexture,
              vixen_texture,
              fl_pixel_buffer_texture_get_type())

static gboolean vixen_texture_copy_pixels(FlPixelBufferTexture* texture,
                                          const uint8_t** out_buffer,
                                          uint32_t* width,
                                          uint32_t* height,
                                          GError** error) {
  VixenTexture* self = VIXEN_TEXTURE(texture);
  g_mutex_lock(&self->mutex);
  const int index = self->current_index;
  if (index < 0) {
    g_mutex_unlock(&self->mutex);
    g_set_error_literal(error, vixen_texture_error_quark(), 1,
                        "No Vixen frame has been published");
    return FALSE;
  }
  *out_buffer = self->buffers[index];
  *width = self->widths[index];
  *height = self->heights[index];
  self->returned_index = index;
  g_mutex_unlock(&self->mutex);
  return TRUE;
}

static void vixen_texture_finalize(GObject* object) {
  VixenTexture* self = VIXEN_TEXTURE(object);
  g_mutex_lock(&self->mutex);
  for (size_t index = 0; index < kPoolSize; ++index) {
    g_clear_pointer(&self->buffers[index], g_free);
    self->capacities[index] = 0;
    self->lengths[index] = 0;
  }
  g_mutex_unlock(&self->mutex);
  g_mutex_clear(&self->mutex);
  G_OBJECT_CLASS(vixen_texture_parent_class)->finalize(object);
}

static void vixen_texture_class_init(VixenTextureClass* klass) {
  G_OBJECT_CLASS(klass)->finalize = vixen_texture_finalize;
  FL_PIXEL_BUFFER_TEXTURE_CLASS(klass)->copy_pixels =
      vixen_texture_copy_pixels;
}

static void vixen_texture_init(VixenTexture* self) {
  g_mutex_init(&self->mutex);
  self->current_index = -1;
  self->returned_index = -1;
}

VixenTexture* vixen_texture_new() {
  return VIXEN_TEXTURE(g_object_new(vixen_texture_get_type(), nullptr));
}

gboolean vixen_texture_publish(VixenTexture* self,
                               uint32_t width,
                               uint32_t height,
                               const uint8_t* rgba,
                               size_t rgba_length,
                               GError** error) {
  g_return_val_if_fail(VIXEN_IS_TEXTURE(self), FALSE);
  if (width == 0 || height == 0 || width > kMaxDimension ||
      height > kMaxDimension) {
    g_set_error_literal(error, vixen_texture_error_quark(), 2,
                        "Frame dimensions are outside Vixen bounds");
    return FALSE;
  }
  const size_t expected_length =
      static_cast<size_t>(width) * static_cast<size_t>(height) * 4U;
  if (rgba == nullptr || expected_length > kMaxFrameBytes ||
      rgba_length != expected_length) {
    g_set_error_literal(error, vixen_texture_error_quark(), 3,
                        "Frame must be exact packed RGBA8");
    return FALSE;
  }

  g_mutex_lock(&self->mutex);
  int target = -1;
  for (size_t index = 0; index < kPoolSize; ++index) {
    const int candidate = static_cast<int>(index);
    if (candidate != self->current_index &&
        candidate != self->returned_index) {
      target = candidate;
      break;
    }
  }
  if (target < 0) {
    g_mutex_unlock(&self->mutex);
    g_set_error_literal(error, vixen_texture_error_quark(), 4,
                        "No safe Vixen texture buffer is available");
    return FALSE;
  }

  if (self->capacities[target] < rgba_length) {
    gpointer resized = g_try_realloc(self->buffers[target], rgba_length);
    if (resized == nullptr) {
      g_mutex_unlock(&self->mutex);
      g_set_error_literal(error, vixen_texture_error_quark(), 5,
                          "Could not allocate Vixen texture buffer");
      return FALSE;
    }
    self->buffers[target] = static_cast<uint8_t*>(resized);
    self->capacities[target] = rgba_length;
  }
  std::memcpy(self->buffers[target], rgba, rgba_length);
  self->lengths[target] = rgba_length;
  self->widths[target] = width;
  self->heights[target] = height;
  self->current_index = target;
  g_mutex_unlock(&self->mutex);
  return TRUE;
}
