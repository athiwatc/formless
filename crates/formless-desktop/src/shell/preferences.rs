use super::*;
use crate::preferences::{Settings, Store};

impl Shell {
    /// Startup attachment; called before interactive input. It performs no write.
    pub fn attach_preferences(
        &mut self,
        store: Store,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut settings = store.settings().clone();
        if !store.has_saved_settings() {
            settings.dark = cx.theme().is_dark();
        }
        self.preference_notice = Some(store.notice().to_string().into());
        self.preferences = Some(store);
        self.apply_preferences(settings, window, cx);
    }

    pub fn sync_system_appearance(&self, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .preferences
            .as_ref()
            .is_some_and(Store::has_saved_settings)
        {
            return;
        }
        Theme::sync_system_appearance(Some(window), cx);
        crate::theme::apply(cx);
    }

    pub fn view_preferences(&self, cx: &App) -> Settings {
        Settings::for_view(
            self.text_scale,
            cx.theme().is_dark(),
            self.prefs.sidebar_open,
            self.prefs.inspector_open,
            self.enter_sends,
        )
    }

    fn apply_preferences(
        &mut self,
        settings: Settings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.invalidate_forecast(cx);
        self.prefs = PanelPrefs {
            sidebar_open: settings.sidebar_open,
            inspector_open: settings.inspector_open,
            ..PanelPrefs::default()
        };
        self.enter_sends = settings.enter_sends;
        self.composer.update(cx, |composer, cx| {
            composer.set_submit_on_enter(self.enter_sends, cx)
        });
        Theme::change(
            if settings.dark {
                ThemeMode::Dark
            } else {
                ThemeMode::Light
            },
            Some(window),
            cx,
        );
        crate::theme::apply(cx);
        self.set_text_scale(TextScale::from_percent(settings.text_scale), window, cx);
        self.settle(window, cx);
    }

    pub(super) fn save_preferences(&mut self, cx: &mut Context<Self>) {
        let settings = self.view_preferences(cx);
        if let Some(store) = &mut self.preferences {
            self.preference_notice = Some(match store.save(settings) {
                Ok(()) => "Desktop view preferences saved. Later changes need another Save. Other windows reload explicitly.".into(),
                Err(error) => error.into(),
            });
            cx.notify();
        }
    }

    pub(super) fn reload_preferences(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(store) = &mut self.preferences else {
            return;
        };
        match store.reload() {
            Ok(settings) => {
                self.preference_notice = Some(store.notice().to_string().into());
                self.apply_preferences(settings, window, cx);
            }
            Err(error) => self.preference_notice = Some(error.into()),
        }
        cx.notify();
    }
}
