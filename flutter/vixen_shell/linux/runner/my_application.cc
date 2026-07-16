#include "my_application.h"

#include <flutter_linux/flutter_linux.h>

#include <atk/atk.h>
#include <cstdio>

#ifdef GDK_WINDOWING_WAYLAND
#include <gdk/gdkwayland.h>
#endif

#include "flutter/generated_plugin_registrant.h"

namespace {

constexpr int64_t kMaxDimension = 4096;
constexpr size_t kMaxViewportBytes = 64U * 1024U * 1024U;

FlView* accessibility_view = nullptr;
decltype(AtkComponentIface::get_extents) flutter_node_get_extents = nullptr;

void FlutterNodeGetExtents(AtkComponent* component,
                           gint* x,
                           gint* y,
                           gint* width,
                           gint* height,
                           AtkCoordType coord_type) {
  AtkObject* parent = atk_object_get_parent(ATK_OBJECT(component));
  if (parent != nullptr &&
      g_str_equal(G_OBJECT_TYPE_NAME(parent), "FlViewAccessible")) {
    // Flutter 3.47 asks its non-component AtkPlug root for extents here. That
    // re-enters the AT-SPI bridge and never returns, so terminate the recursive
    // walk at the view-local root while retaining Flutter's descendant bounds.
    if (x != nullptr) {
      *x = 0;
    }
    if (y != nullptr) {
      *y = 0;
    }
    if (width != nullptr) {
      *width = accessibility_view == nullptr
                   ? 0
                   : gtk_widget_get_allocated_width(
                         GTK_WIDGET(accessibility_view));
    }
    if (height != nullptr) {
      *height = accessibility_view == nullptr
                    ? 0
                    : gtk_widget_get_allocated_height(
                          GTK_WIDGET(accessibility_view));
    }
    return;
  }
  flutter_node_get_extents(component, x, y, width, height, coord_type);
}

void PatchFlutterNodeExtents(FlEngine*, gpointer, gpointer) {
  GType accessible_type = g_type_from_name("FlAccessibleNode");
  if (accessible_type == G_TYPE_INVALID) {
    g_warning("Flutter node accessibility type is unavailable");
    return;
  }
  gpointer accessible_class = g_type_class_ref(accessible_type);
  auto* component_interface = static_cast<AtkComponentIface*>(
      g_type_interface_peek(accessible_class, ATK_TYPE_COMPONENT));
  if (component_interface == nullptr ||
      component_interface->get_extents == nullptr) {
    g_warning("Flutter node accessibility component is unavailable");
  } else if (component_interface->get_extents != FlutterNodeGetExtents) {
    flutter_node_get_extents = component_interface->get_extents;
    component_interface->get_extents = FlutterNodeGetExtents;
  }
  g_type_class_unref(accessible_class);
}

bool HasDartArgument(char** arguments, const char* expected) {
  if (arguments == nullptr) {
    return false;
  }
  for (size_t index = 0; arguments[index] != nullptr; index++) {
    if (g_str_equal(arguments[index], expected)) {
      return true;
    }
  }
  return false;
}

bool AutomationViewport(char** arguments, int* width, int* height) {
  if (arguments == nullptr) {
    return false;
  }
  constexpr char kPrefix[] = "--vixen-viewport=";
  for (size_t index = 0; arguments[index] != nullptr; index++) {
    const char* argument = arguments[index];
    if (!g_str_has_prefix(argument, kPrefix)) {
      continue;
    }
    unsigned int parsed_width = 0;
    unsigned int parsed_height = 0;
    char trailing = '\0';
    if (sscanf(argument + sizeof(kPrefix) - 1, "%ux%u%c", &parsed_width,
               &parsed_height, &trailing) != 2 ||
        parsed_width == 0 || parsed_height == 0 ||
        parsed_width > kMaxDimension || parsed_height > kMaxDimension ||
        static_cast<size_t>(parsed_width) *
                static_cast<size_t>(parsed_height) * 4U >
            kMaxViewportBytes) {
      return false;
    }
    *width = static_cast<int>(parsed_width);
    *height = static_cast<int>(parsed_height);
    return true;
  }
  return false;
}

}  // namespace

struct _MyApplication {
  GtkApplication parent_instance;
  char** dart_entrypoint_arguments;
  FlView* view;
};

G_DEFINE_TYPE(MyApplication, my_application, GTK_TYPE_APPLICATION)

// Called when first Flutter frame received.
static void first_frame_cb(MyApplication* self, FlView* view) {
  gtk_widget_show(gtk_widget_get_toplevel(GTK_WIDGET(view)));
}

// Implements GApplication::activate.
static void my_application_activate(GApplication* application) {
  MyApplication* self = MY_APPLICATION(application);
  GtkWindow* window =
      GTK_WINDOW(gtk_application_window_new(GTK_APPLICATION(application)));

  const bool automation =
      HasDartArgument(self->dart_entrypoint_arguments, "--vixen-automation") ||
      HasDartArgument(self->dart_entrypoint_arguments,
                      "--vixen-cdp-automation");
  const bool headless_window = HasDartArgument(
      self->dart_entrypoint_arguments, "--vixen-headless-window");

  if (automation || headless_window) {
    gtk_window_set_decorated(window, FALSE);
    gtk_window_set_resizable(window, FALSE);
    int width = 0;
    int height = 0;
    if (AutomationViewport(self->dart_entrypoint_arguments, &width, &height)) {
      gtk_window_set_default_size(window, width, height);
    }
  } else {
    // Yaru hides this host header after its in-scene title bar initializes. Keep
    // it as a native fallback if Dart or plugin startup fails.
    GtkHeaderBar* header_bar = GTK_HEADER_BAR(gtk_header_bar_new());
    gtk_widget_show(GTK_WIDGET(header_bar));
    gtk_header_bar_set_title(header_bar, "Vixen");
    gtk_header_bar_set_show_close_button(header_bar, TRUE);
    gtk_window_set_titlebar(window, GTK_WIDGET(header_bar));
    gtk_window_set_default_size(window, 1100, 820);
  }

  g_autoptr(FlDartProject) project = fl_dart_project_new();
  // Flutter 3.47 beta contains Linux Impeller but its project default remains
  // false in this release tag. Enable it explicitly for packaged and local
  // runner launches rather than relying on a flutter-tool-only run flag.
  fl_dart_project_set_enable_impeller(project, TRUE);
  fl_dart_project_set_dart_entrypoint_arguments(
      project, self->dart_entrypoint_arguments);

  self->view = fl_view_new(project);
  accessibility_view = self->view;
  g_signal_connect(fl_view_get_engine(self->view), "update-semantics",
                   G_CALLBACK(PatchFlutterNodeExtents), nullptr);
  fl_register_plugins(FL_PLUGIN_REGISTRY(self->view));
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

#ifdef GDK_WINDOWING_WAYLAND
  GdkDisplay* display = gdk_display_get_default();
  const gboolean is_wayland =
      display != nullptr && GDK_IS_WAYLAND_DISPLAY(display);
#else
  const gboolean is_wayland = FALSE;
#endif
  if (!is_wayland) {
    g_printerr("Vixen requires a native Wayland session; X11 and XWayland are "
               "unsupported.\n");
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
  G_APPLICATION_CLASS(my_application_parent_class)->shutdown(application);
}

// Implements GObject::dispose.
static void my_application_dispose(GObject* object) {
  MyApplication* self = MY_APPLICATION(object);
  accessibility_view = nullptr;
  g_clear_pointer(&self->dart_entrypoint_arguments, g_strfreev);
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
