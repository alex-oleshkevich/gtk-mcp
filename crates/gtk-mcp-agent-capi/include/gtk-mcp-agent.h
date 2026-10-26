/* gtk-mcp-agent.h: C API of the gtk-mcp embedded agent (libgtk_mcp_agent_capi).
 *
 * Every function must be called on the GTK main thread, after GTK is
 * initialized, and is inert unless the process environment has GTK_MCP=1.
 * The library links the host's libgtk-4 dynamically; it does not embed GTK.
 *
 * Kept in sync with src/lib.rs and GtkMcp-1.gir by the test
 * crates/gtk-mcp/tests/fixture_m7.rs.
 */
#ifndef GTK_MCP_AGENT_H
#define GTK_MCP_AGENT_H

#include <gtk/gtk.h>

G_BEGIN_DECLS

/**
 * GtkMcpStateFunc:
 * @user_data: the pointer given to gtk_mcp_agent_register_state()
 *
 * Returns: (transfer full) (nullable): a newly allocated JSON document (free
 *   with g_free()); NULL means JSON `null`.
 */
typedef char *(*GtkMcpStateFunc) (gpointer user_data);

/** Attach the agent to @app. Idempotent; inert unless GTK_MCP=1. */
void gtk_mcp_agent_attach (GtkApplication *app);

/** Mark @object (normally a widget) as secret: its values are never returned. */
void gtk_mcp_agent_mark_secret (GObject *object);

/** Make the group inserted on @widget under @prefix visible to the agent's action tools. */
void gtk_mcp_agent_register_action_group (GtkWidget *widget, const char *prefix, GActionGroup *group);

/** Tell the agent about a GSettings object the application uses. */
void gtk_mcp_agent_register_settings (GSettings *settings);

/**
 * Expose the JSON returned by @callback as the read-only MCP resource
 * `app-state/<name>`. @destroy is called with @user_data when the
 * registration is replaced, or immediately when the agent is inactive.
 */
void gtk_mcp_agent_register_state (const char *name,
                                   GtkMcpStateFunc callback,
                                   gpointer user_data,
                                   GDestroyNotify destroy);

G_END_DECLS

#endif /* GTK_MCP_AGENT_H */
