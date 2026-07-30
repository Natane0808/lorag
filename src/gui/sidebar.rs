//! Left-hand navigation sidebar for the desktop launcher window.
//!
//! Renders a [`gpui_component::sidebar::Sidebar`] with one menu item per
//! [`Page`]. The active item is highlighted from
//! [`AppState::current_page`], and clicking an item calls
//! [`AppState::switch_page`] which notifies the root view to swap the
//! right-hand content pane.

use gpui::prelude::*;
use gpui::{App, IntoElement, Window};
use gpui_component::sidebar::{
    Sidebar, SidebarFooter, SidebarGroup, SidebarHeader, SidebarMenu, SidebarMenuItem,
};

use super::app::AppState;
use super::pages::ALL_PAGES;

/// Build the sidebar element. `state` is the global [`AppState`] entity;
/// click handlers call `state.update(cx, |s, cx| s.switch_page(page, cx))`.
pub fn render_sidebar(
    state: &gpui::Entity<AppState>,
    _window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    let current = state.read(cx).current_page;

    Sidebar::new("lorag-sidebar")
        .header(SidebarHeader::new().child("lorag"))
        .child(
            SidebarGroup::new("Navigation").child(SidebarMenu::new().children(
                ALL_PAGES.iter().map(|page| {
                    let page = *page;
                    let state = state.clone();
                    SidebarMenuItem::new(page.title_cn())
                        .active(current == page)
                        .on_click(move |_ev, _window, cx: &mut App| {
                            state.update(cx, |s, cx| s.switch_page(page, cx));
                        })
                }),
            )),
        )
        .footer(SidebarFooter::new().child("lorag · v0.1"))
}
