#include "my_application.h"

#include <flutter_linux/flutter_linux.h>
#ifdef GDK_WINDOWING_X11
#include <gdk/gdkx.h>
#endif

#include "flutter/generated_plugin_registrant.h"
#include "vixen_texture.h"

namespace {

constexpr char kTextureChannel[] = "org.vixen.Vixen/texture";
constexpr int64_t kMaxDimension = 4096;
constexpr size_t kMaxFrameBytes = 64U * 1024U * 1024U;

}  // namespace

struct _MyApplication {
  GtkApplication parent_instance;
  char** dart_entrypoint_arguments;
  FlMethodChannel* texture_channel;
  FlTextureRegistrar* texture_registrar;
  VixenTexture* texture;
  FlView* view;
};

G_DEFINE_TYPE(MyApplication, my_application, GTK_TYPE_APPLICATION)

static FlMethodResponse* method_error(const char* code, const char* message) {
  return FL_METHOD_RESPONSE(
      fl_method_error_response_new(code, message, nullptr));
}

static void unregister_texture(MyApplication* self) {
  if (self->texture == nullptr) {
    return;
  }
  if (self->texture_registrar != nullptr) {
    fl_texture_registrar_unregister_texture(self->texture_registrar,
                                            FL_TEXTURE(self->texture));
  }
  g_clear_object(&self->texture);
}

static FlMethodResponse* handle_texture_create(MyApplication* self,
                                               FlMethodCall* method_call) {
  FlValue* args = fl_method_call_get_args(method_call);
  if (fl_value_get_type(args) != FL_VALUE_TYPE_NULL) {
    return method_error("texture.invalid-arguments",
                        "create does not accept arguments");
  }
  if (self->texture != nullptr) {
    return method_error("texture.already-created",
                        "the window texture was already created");
  }
  self->texture = vixen_texture_new();
  if (!fl_texture_registrar_register_texture(self->texture_registrar,
                                             FL_TEXTURE(self->texture))) {
    g_clear_object(&self->texture);
    return method_error("texture.registration-failed",
                        "could not register the window texture");
  }
  g_autoptr(FlValue) result =
      fl_value_new_int(fl_texture_get_id(FL_TEXTURE(self->texture)));
  return FL_METHOD_RESPONSE(fl_method_success_response_new(result));
}

static FlMethodResponse* handle_texture_publish(MyApplication* self,
                                                FlMethodCall* method_call) {
  if (self->texture == nullptr) {
    return method_error("texture.not-created",
                        "publish requires a created texture");
  }
  FlValue* args = fl_method_call_get_args(method_call);
  if (fl_value_get_type(args) != FL_VALUE_TYPE_MAP ||
      fl_value_get_length(args) != 3) {
    return method_error("texture.invalid-arguments",
                        "publish requires width, height, and rgba");
  }
  FlValue* width_value = fl_value_lookup_string(args, "width");
  FlValue* height_value = fl_value_lookup_string(args, "height");
  FlValue* rgba_value = fl_value_lookup_string(args, "rgba");
  if (width_value == nullptr || height_value == nullptr ||
      rgba_value == nullptr ||
      fl_value_get_type(width_value) != FL_VALUE_TYPE_INT ||
      fl_value_get_type(height_value) != FL_VALUE_TYPE_INT ||
      fl_value_get_type(rgba_value) != FL_VALUE_TYPE_UINT8_LIST) {
    return method_error("texture.invalid-arguments",
                        "publish payload has invalid field types");
  }
  const int64_t width_value_int = fl_value_get_int(width_value);
  const int64_t height_value_int = fl_value_get_int(height_value);
  if (width_value_int <= 0 || height_value_int <= 0 ||
      width_value_int > kMaxDimension || height_value_int > kMaxDimension) {
    return method_error("texture.invalid-dimensions",
                        "publish dimensions exceed 4096 pixels");
  }
  const size_t width = static_cast<size_t>(width_value_int);
  const size_t height = static_cast<size_t>(height_value_int);
  const size_t expected_length = width * height * 4U;
  const size_t rgba_length = fl_value_get_length(rgba_value);
  if (expected_length > kMaxFrameBytes || rgba_length != expected_length) {
    return method_error("texture.invalid-pixels",
                        "rgba must contain exact packed RGBA8 bytes");
  }
  g_autoptr(GError) error = nullptr;
  if (!vixen_texture_publish(
          self->texture, static_cast<uint32_t>(width),
          static_cast<uint32_t>(height), fl_value_get_uint8_list(rgba_value),
          rgba_length, &error)) {
    return method_error("texture.publish-failed", error->message);
  }
  if (!fl_texture_registrar_mark_texture_frame_available(
          self->texture_registrar, FL_TEXTURE(self->texture))) {
    return method_error("texture.frame-notification-failed",
                        "could not notify Flutter of the texture frame");
  }
  return FL_METHOD_RESPONSE(fl_method_success_response_new(nullptr));
}

static FlMethodResponse* handle_texture_dispose(MyApplication* self,
                                                FlMethodCall* method_call) {
  FlValue* args = fl_method_call_get_args(method_call);
  if (fl_value_get_type(args) != FL_VALUE_TYPE_NULL) {
    return method_error("texture.invalid-arguments",
                        "dispose does not accept arguments");
  }
  if (self->texture == nullptr) {
    return method_error("texture.not-created",
                        "there is no window texture to dispose");
  }
  unregister_texture(self);
  return FL_METHOD_RESPONSE(fl_method_success_response_new(nullptr));
}

static void texture_channel_method_cb(FlMethodChannel* channel,
                                      FlMethodCall* method_call,
                                      gpointer user_data) {
  (void)channel;
  MyApplication* self = MY_APPLICATION(user_data);
  const char* method = fl_method_call_get_name(method_call);
  g_autoptr(FlMethodResponse) response = nullptr;
  if (g_str_equal(method, "create")) {
    response = handle_texture_create(self, method_call);
  } else if (g_str_equal(method, "publish")) {
    response = handle_texture_publish(self, method_call);
  } else if (g_str_equal(method, "dispose")) {
    response = handle_texture_dispose(self, method_call);
  } else {
    fl_method_call_respond_not_implemented(method_call, nullptr);
    return;
  }
  fl_method_call_respond(method_call, response, nullptr);
}

// Called when first Flutter frame received.
static void first_frame_cb(MyApplication* self, FlView* view) {
  gtk_widget_show(gtk_widget_get_toplevel(GTK_WIDGET(view)));
}

// Implements GApplication::activate.
static void my_application_activate(GApplication* application) {
  MyApplication* self = MY_APPLICATION(application);
  GtkWindow* window =
      GTK_WINDOW(gtk_application_window_new(GTK_APPLICATION(application)));

  // Use a header bar when running in GNOME as this is the common style used
  // by applications and is the setup most users will be using (e.g. Ubuntu
  // desktop).
  // If running on X and not using GNOME then just use a traditional title bar
  // in case the window manager does more exotic layout, e.g. tiling.
  // If running on Wayland assume the header bar will work (may need changing
  // if future cases occur).
  gboolean use_header_bar = TRUE;
#ifdef GDK_WINDOWING_X11
  GdkScreen* screen = gtk_window_get_screen(window);
  if (GDK_IS_X11_SCREEN(screen)) {
    const gchar* wm_name = gdk_x11_screen_get_window_manager_name(screen);
    if (g_strcmp0(wm_name, "GNOME Shell") != 0) {
      use_header_bar = FALSE;
    }
  }
#endif
  if (use_header_bar) {
    GtkHeaderBar* header_bar = GTK_HEADER_BAR(gtk_header_bar_new());
    gtk_widget_show(GTK_WIDGET(header_bar));
    gtk_header_bar_set_title(header_bar, "Vixen");
    gtk_header_bar_set_show_close_button(header_bar, TRUE);
    gtk_window_set_titlebar(window, GTK_WIDGET(header_bar));
  } else {
    gtk_window_set_title(window, "Vixen");
  }

  gtk_window_set_default_size(window, 1100, 820);

  g_autoptr(FlDartProject) project = fl_dart_project_new();
  // Flutter 3.46 beta contains Linux Impeller but its project default remains
  // false in this release tag. Enable it explicitly for packaged and local
  // runner launches rather than relying on a flutter-tool-only run flag.
  fl_dart_project_set_enable_impeller(project, TRUE);
  fl_dart_project_set_dart_entrypoint_arguments(
      project, self->dart_entrypoint_arguments);

  self->view = fl_view_new(project);
  GdkRGBA background_color;
  // Background defaults to black, override it here if necessary, e.g. #00000000
  // for transparent.
  gdk_rgba_parse(&background_color, "#000000");
  fl_view_set_background_color(self->view, &background_color);
  gtk_widget_show(GTK_WIDGET(self->view));
  gtk_container_add(GTK_CONTAINER(window), GTK_WIDGET(self->view));

  // Show the window when Flutter renders.
  // Requires the view to be realized so we can start rendering.
  g_signal_connect_swapped(self->view, "first-frame", G_CALLBACK(first_frame_cb),
                           self);
  gtk_widget_realize(GTK_WIDGET(self->view));

  FlEngine* engine = fl_view_get_engine(self->view);
  self->texture_registrar = FL_TEXTURE_REGISTRAR(
      g_object_ref(fl_engine_get_texture_registrar(engine)));
  g_autoptr(FlStandardMethodCodec) codec = fl_standard_method_codec_new();
  self->texture_channel = fl_method_channel_new(
      fl_engine_get_binary_messenger(engine), kTextureChannel,
      FL_METHOD_CODEC(codec));
  fl_method_channel_set_method_call_handler(
      self->texture_channel, texture_channel_method_cb, self, nullptr);

  fl_register_plugins(FL_PLUGIN_REGISTRY(self->view));

  gtk_widget_grab_focus(GTK_WIDGET(self->view));
}

// Implements GApplication::local_command_line.
static gboolean my_application_local_command_line(GApplication* application,
                                                  gchar*** arguments,
                                                  int* exit_status) {
  MyApplication* self = MY_APPLICATION(application);
  // Strip out the first argument as it is the binary name.
  self->dart_entrypoint_arguments = g_strdupv(*arguments + 1);

  g_autoptr(GError) error = nullptr;
  if (!g_application_register(application, nullptr, &error)) {
    g_warning("Failed to register: %s", error->message);
    *exit_status = 1;
    return TRUE;
  }

  g_application_activate(application);
  *exit_status = 0;

  return TRUE;
}

// Implements GApplication::startup.
static void my_application_startup(GApplication* application) {
  // MyApplication* self = MY_APPLICATION(object);

  // Perform any actions required at application startup.

  G_APPLICATION_CLASS(my_application_parent_class)->startup(application);
}

// Implements GApplication::shutdown.
static void my_application_shutdown(GApplication* application) {
  MyApplication* self = MY_APPLICATION(application);
  if (self->texture_channel != nullptr) {
    fl_method_channel_set_method_call_handler(self->texture_channel, nullptr,
                                              nullptr, nullptr);
  }
  unregister_texture(self);

  G_APPLICATION_CLASS(my_application_parent_class)->shutdown(application);
}

// Implements GObject::dispose.
static void my_application_dispose(GObject* object) {
  MyApplication* self = MY_APPLICATION(object);
  g_clear_pointer(&self->dart_entrypoint_arguments, g_strfreev);
  unregister_texture(self);
  g_clear_object(&self->texture_channel);
  g_clear_object(&self->texture_registrar);
  G_OBJECT_CLASS(my_application_parent_class)->dispose(object);
}

static void my_application_class_init(MyApplicationClass* klass) {
  G_APPLICATION_CLASS(klass)->activate = my_application_activate;
  G_APPLICATION_CLASS(klass)->local_command_line =
      my_application_local_command_line;
  G_APPLICATION_CLASS(klass)->startup = my_application_startup;
  G_APPLICATION_CLASS(klass)->shutdown = my_application_shutdown;
  G_OBJECT_CLASS(klass)->dispose = my_application_dispose;
}

static void my_application_init(MyApplication* self) {}

MyApplication* my_application_new() {
  // Set the program name to the application ID, which helps various systems
  // like GTK and desktop environments map this running application to its
  // corresponding .desktop file. This ensures better integration by allowing
  // the application to be recognized beyond its binary name.
  g_set_prgname(APPLICATION_ID);

  return MY_APPLICATION(g_object_new(my_application_get_type(),
                                     "application-id", APPLICATION_ID, "flags",
                                     G_APPLICATION_NON_UNIQUE, nullptr));
}
