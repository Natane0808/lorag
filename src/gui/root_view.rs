//! Window root view: custom title bar on top, sidebar on the left, current
//! page on the right.
//!
//! [`RootView`] owns an [`Entity<AppState>`] and re-renders the right-hand
//! pane whenever [`AppState::current_page`] changes (driven by sidebar click
//! handlers calling [`super::app::AppState::switch_page`]).

use gpui::prelude::*;
use gpui::{App, Context, Entity, IntoElement, MouseButton, Render, Window, div};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{ActiveTheme as _, IconName, Sizable, Theme, ThemeMode, TitleBar};

use super::about::AboutPage;
use super::app::AppState;
use super::doctor::DoctorPage;
use super::ingest::IngestPage;
use super::models::ModelsPage;
use super::pages::Page;
use super::service::ServicePage;
use super::settings::SettingsPage;
use super::sidebar::render_sidebar;

/// Top-level view rendered inside [`gpui_component::Root`] for the main window.
pub struct RootView {
    /// Shared global state entity; sidebar + pages all read/write through it.
    pub state: Entity<AppState>,
    /// G5: the live service-control entity view. We instantiate it once on
    /// RootView construction and reuse it across re-renders so the service
    /// state machine survives page switches.
    service_page: Entity<ServicePage>,
    /// G6: the live model-management entity view. Same pattern as G5 — built
    /// once, held across page switches, so in-flight downloads survive.
    models_page: Entity<ModelsPage>,
    /// G7: the live document-ingest entity view. Same pattern — built once so
    /// the pending-file queue and in-flight ingest tasks survive page swaps.
    ingest_page: Entity<IngestPage>,
    /// G8: the live doctor health-check entity view. Built once so the
    /// auto-run on first construction only fires once (not on every re-render)
    /// and the last-run timestamp persists across sidebar switches.
    doctor_page: Entity<DoctorPage>,
    /// G10: the live settings form entity view. Built once so partial edits
    /// survive sidebar navigation (draft isn't lost when clicking over to
    /// Service/Models and back).
    settings_page: Entity<SettingsPage>,
    /// G11: the static about page entity. Stateless but built once for
    /// consistency with the other page holders; its render is pure over
    /// the theme so there is no runtime cost to reusing the entity.
    about_page: Entity<AboutPage>,
}

impl RootView {
    /// Construct a new root view wrapping the given [`AppState`] entity.
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let service_page = cx.new(|cx| ServicePage::new(state.clone(), window, cx));
        let models_page = cx.new(|cx| ModelsPage::new(state.clone(), window, cx));
        let ingest_page = cx.new(|cx| IngestPage::new(state.clone(), window, cx));
        let doctor_page = cx.new(|cx| DoctorPage::new(state.clone(), window, cx));
        let settings_page = cx.new(|cx| SettingsPage::new(state.clone(), window, cx));
        let about_page = cx.new(|cx| AboutPage::new(state.clone(), window, cx));
        Self {
            state,
            service_page,
            models_page,
            ingest_page,
            doctor_page,
            settings_page,
            about_page,
        }
    }
}

impl Render for RootView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.state.read(cx).current_page;
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .child(
                TitleBar::new()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().text_sm().child("lorag"))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("v{}", env!("CARGO_PKG_VERSION"))),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .pr_2()
                            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                cx.stop_propagation();
                            })
                            .child(
                                Button::new("titlebar-theme")
                                    .small()
                                    .ghost()
                                    .icon(if cx.theme().is_dark() {
                                        IconName::Sun
                                    } else {
                                        IconName::Moon
                                    })
                                    .on_click(|_ev, _window, cx: &mut App| {
                                        let mode = if cx.theme().is_dark() {
                                            ThemeMode::Light
                                        } else {
                                            ThemeMode::Dark
                                        };
                                        Theme::change(mode, None, cx);
                                    }),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(render_sidebar(&self.state, window, cx))
                    .child(div().flex_1().h_full().min_w_0().child(render_page(
                        current,
                        &self.service_page,
                        &self.models_page,
                        &self.ingest_page,
                        &self.doctor_page,
                        &self.settings_page,
                        &self.about_page,
                    ))),
            )
    }
}

/// Render the right-hand content pane for `page`.
///
/// G5–G11: [`Page::Service`] / [`Page::Models`] / [`Page::Ingest`] /
/// [`Page::Doctor`] / [`Page::Settings`] / [`Page::About`] each render their
/// live/static entity view held on [`RootView`]. The live log viewer (G9) is
/// embedded inside the Service page rather than having its own sidebar entry.
fn render_page(
    page: Page,
    service_page: &Entity<ServicePage>,
    models_page: &Entity<ModelsPage>,
    ingest_page: &Entity<IngestPage>,
    doctor_page: &Entity<DoctorPage>,
    settings_page: &Entity<SettingsPage>,
    about_page: &Entity<AboutPage>,
) -> gpui::AnyElement {
    match page {
        Page::Service => service_page.clone().into_any_element(),
        Page::Models => models_page.clone().into_any_element(),
        Page::Ingest => ingest_page.clone().into_any_element(),
        Page::Doctor => doctor_page.clone().into_any_element(),
        Page::Settings => settings_page.clone().into_any_element(),
        Page::About => about_page.clone().into_any_element(),
    }
}
