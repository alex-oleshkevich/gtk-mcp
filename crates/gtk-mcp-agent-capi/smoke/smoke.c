/* C smoke app: attaches the gtk-mcp agent through the C API.
 * cc smoke.c -I../include $(pkg-config --cflags --libs gtk4) -L<dir> -lgtk_mcp_agent_capi */
#include <gtk-mcp-agent.h>

#define TOKEN "c-hunter2-TOKEN-0xC0FFEE"

static char *
state_cb (gpointer user_data)
{
  return g_strdup_printf ("{\"lang\":\"c\",\"tag\":\"%s\"}", (const char *) user_data);
}

static void
ping (GSimpleAction *action, GVariant *param, gpointer user_data)
{
}

static void
activate (GtkApplication *app, gpointer user_data)
{
  GtkWidget *win = gtk_application_window_new (app);
  GtkWidget *panel = gtk_box_new (GTK_ORIENTATION_VERTICAL, 6);
  GtkWidget *label = gtk_label_new ("hello from c");
  GtkWidget *secret = gtk_entry_new ();
  GSimpleActionGroup *group = g_simple_action_group_new ();
  GSimpleAction *act = g_simple_action_new ("ping", NULL);

  gtk_widget_set_name (panel, "smoke-panel");
  gtk_widget_set_name (label, "smoke-label");
  gtk_widget_set_name (secret, "smoke-secret");
  gtk_editable_set_text (GTK_EDITABLE (secret), TOKEN);
  gtk_mcp_agent_mark_secret (G_OBJECT (secret));
  g_signal_connect (act, "activate", G_CALLBACK (ping), NULL);
  g_action_map_add_action (G_ACTION_MAP (group), G_ACTION (act));
  gtk_widget_insert_action_group (panel, "smoke", G_ACTION_GROUP (group));
  gtk_mcp_agent_register_action_group (panel, "smoke", G_ACTION_GROUP (group));
  gtk_box_append (GTK_BOX (panel), label);
  gtk_box_append (GTK_BOX (panel), secret);
  gtk_window_set_child (GTK_WINDOW (win), panel);
  gtk_window_present (GTK_WINDOW (win));
}

int
main (int argc, char **argv)
{
  GtkApplication *app = gtk_application_new ("dev.gtkmcp.SmokeC", G_APPLICATION_DEFAULT_FLAGS);
  int status;

  g_signal_connect (app, "activate", G_CALLBACK (activate), NULL);
  gtk_mcp_agent_attach (app);
  gtk_mcp_agent_register_state ("smoke", state_cb, g_strdup ("one"), g_free);
  status = g_application_run (G_APPLICATION (app), argc, argv);
  g_object_unref (app);
  return status;
}
