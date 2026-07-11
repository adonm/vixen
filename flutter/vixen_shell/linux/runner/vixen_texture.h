#ifndef FLUTTER_VIXEN_TEXTURE_H_
#define FLUTTER_VIXEN_TEXTURE_H_

#include <stddef.h>
#include <stdint.h>

#include <flutter_linux/flutter_linux.h>

G_DECLARE_FINAL_TYPE(VixenTexture,
                     vixen_texture,
                     VIXEN,
                     TEXTURE,
                     FlPixelBufferTexture)

VixenTexture* vixen_texture_new();

gboolean vixen_texture_publish(VixenTexture* self,
                               uint32_t width,
                               uint32_t height,
                               const uint8_t* rgba,
                               size_t rgba_length,
                               GError** error);

#endif  // FLUTTER_VIXEN_TEXTURE_H_
