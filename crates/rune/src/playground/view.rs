//! The playground's window: the bar, the sidebar of stories, the stage and the side column.

use super::*;

impl<S: 'static> Live<S> {
    /// The window's root: the app's view above the bar, or the playground around its stage.
    pub(crate) fn view(&self, app: &S, view: fn(&S, &mut Cx<S>) -> El, cx: &mut Cx<S>, viewport: (f32, f32)) -> El {
        let dark = self.app_dark.map_or_else(|| cx.system_dark(), |dark| dark(app));
        let t = tok::theme(dark);
        if !self.open {
            if !self.bar { return call(view, app, cx); }
            let root = call(view, app, cx);
            // Laid out as the root of a window BAR shorter, as the runtime sizes a root. The
            // runtime finds the root's handlers on it here (`Runtime::app_root`).
            return col().key("playground-closed").w(viewport.0).h(viewport.1)
                .child(root.w(viewport.0).h((viewport.1 - BAR).max(0.0)))
                .child(self.bar_view(t));
        }
        let stage_w = (viewport.0 - SIDEBAR - SIDE - 2.0).max(0.0);
        let stage = self.stage(app, view, cx, (stage_w, viewport.1), t, dark);
        let side = self.side(app, cx, t, dark, viewport.1);
        row().key("playground").w(viewport.0).h(viewport.1).fill(t.bg)
            .child(self.sidebar(t, dark, cx.titlebar_inset(), viewport.1))
            .child(el().w(1.0).h(viewport.1).fill(t.separator))
            .child(stage)
            .child(el().w(1.0).h(viewport.1).fill(t.separator))
            .child(side)
    }

    fn bar_view(&self, t: Theme) -> El {
        let stories = match self.order.len() { 0 => String::new(), 1 => "1 story".into(), n => format!("{n} stories") };
        let hint = self.shortcut.as_ref().map(keycaps);
        let summary = [Some(stories).filter(|s| !s.is_empty()), hint].into_iter().flatten().collect::<Vec<_>>().join(" · ");
        let open = row().key("playground-open").h(20.0).ml(4.0).px(6.0).gap(6.0).center_y().radius(radius::SM - 1.0)
            .hover(t.hover).press(t.press)
            .child(icon(&GRID, 12.0).color(t.text_secondary))
            .child(text("Playground").font(type_::LABEL).color(t.text_secondary))
            .role("button").label("Open the playground")
            .on_press_tx(|_: &mut Controls, tx: &mut Tx| tx.open_playground());
        col().w_full().h(BAR).fill(t.bg)
            .child(el().w_full().h(1.0).fill(t.separator))
            .child(
                row().w_full().h(BAR - 1.0).center_y().justify_between()
                    .child(open)
                    .child(text(summary).font(type_::LABEL).color(t.text_secondary).pr(10.0)),
            )
    }

    fn sidebar(&self, t: Theme, dark: bool, inset: f32, height: f32) -> El {
        let back = row().key("playground-back").h(CONTROL).pl(4.0).pr(8.0).gap(2.0).center_y().radius(radius::SM)
            .hover(t.hover).press(t.press)
            .child(icon(&CHEVRON_LEFT, 16.0).color(t.text_secondary))
            .child(text("Back").font(type_::LABEL).color(t.text))
            .role("button").label("Back to the app")
            .on_press_tx(|_: &mut Controls, tx: &mut Tx| tx.close_playground());
        // Back sits at the list's text inset; where the window's own controls take the corner,
        // it goes in the row under them.
        let band = if inset > 0.0 { BAND } else { 0.0 };
        let header = row().w(SIDEBAR).h(HEADER).pl(8.0).center_y().child(back);
        let drag = (band > 0.0).then(|| el().w(SIDEBAR).h(band).window_drag());
        // A scroll measures its first child, so the rows go in one content column.
        let mut list = col().key("playground-rows").w_full();
        for (n, entry) in self.rows().into_iter().enumerate() {
            let (on, name, item) = match entry {
                Row::Header(group, i) => {
                    list = list.child(self.header_row(t, dark, group, i, n == 0));
                    continue;
                }
                Row::Story(i) => (i == self.selected && self.header.is_none(), self.name(i), i),
            };
            list = list.child(
                row().key(format!("playground-item-{item}")).w_full().h(ROW).px(8.0).center_y().radius(radius::MD)
                    .fill(if on { selected(t, dark) } else { Color::TRANSPARENT })
                    .hover(if on { selected(t, dark) } else { t.hover })
                    .child(text(name).font(type_::BODY).color(if on || item == self.selected { t.text } else { t.text_secondary }).truncate())
                    .role("button").label(name)
                    .on_press_tx(move |_: &mut Controls, tx: &mut Tx| tx.playground.push(Request::Select(item))),
            );
        }
        if self.order.is_empty() {
            list = list.child(text("No stories").font(type_::BODY).color(t.text_secondary).px(8.0).pt(4.0));
        }
        let list = col().key("playground-list").w(SIDEBAR).h(height - 2.0 * HEADER - band).px(8.0).pb(8.0)
            .scroll(ScrollSpec { stick_bottom: false }).child(list);
        col().w(SIDEBAR).h(height)
            .children(drag)
            .child(header)
            .child(list)
            .child(self.stage_controls(t, dark))
    }

    /// A group's header, keyed by its first story: a press selects it and folds or unfolds it.
    fn header_row(&self, t: Theme, dark: bool, group: &'static str, first: usize, top: bool) -> El {
        let on = self.header == Some(group);
        let folded = self.folded.contains(&group);
        let row = row().key(format!("playground-group-{first}")).w_full().h(24.0).px(8.0).gap(4.0).center_y().radius(radius::MD)
            .fill(if on { selected(t, dark) } else { Color::TRANSPARENT })
            .hover(if on { selected(t, dark) } else { t.hover })
            .child(text(group).font(type_::SECTION).color(if on { t.text } else { t.text_secondary }).truncate())
            .child(spacer())
            .child(icon(if folded { &CHEVRON_RIGHT } else { &CHEVRON_DOWN }, 14.0).color(t.text_secondary))
            .role("button").label(if folded { format!("Show {group}") } else { format!("Hide {group}") })
            .on_press_tx(move |_: &mut Controls, tx: &mut Tx| {
                tx.playground.push(Request::Header(group));
                tx.playground.push(Request::Fold(group, None));
            });
        col().w_full().pt(if top { 2.0 } else { 10.0 }).pb(2.0).child(row)
    }

    /// Along the sidebar's foot: the stage's size, motion and appearance.
    fn stage_controls(&self, t: Theme, window_dark: bool) -> El {
        let dark = self.dark.unwrap_or(window_dark);
        let (glyph, appearance) = if dark { (&MOON, "Dark appearance") } else { (&SUN, "Light appearance") };
        let theme = icon_button(t, "playground-theme", glyph, appearance, false, window_dark, Request::Dark(!dark));
        let reduce = icon_button(t, "playground-reduce-motion", &MOTION, "Reduce Motion", self.reduce_motion, window_dark, Request::ReduceMotion(!self.reduce_motion));
        let sizes = (!self.sizes.is_empty()).then(|| self.size_menu(t));
        row().key("playground-stage-controls").w(SIDEBAR).h(HEADER).px(8.0).gap(2.0).center_y()
            .children(sizes)
            .child(spacer())
            .child(reduce)
            .child(theme)
    }

    /// The stage's size: the window's room or a preset, from a pop-up menu (a press steps
    /// through them where there's no native one).
    fn size_menu(&self, t: Theme) -> El {
        let current = self.size.checked_sub(1).and_then(|i| self.sizes.get(i)).map_or("Window", |(n, _)| *n);
        let options: Vec<(String, String)> = std::iter::once(("0".to_owned(), "Window".to_owned()))
            .chain(self.sizes.iter().enumerate().map(|(i, (n, (w, h)))| ((i + 1).to_string(), format!("{n}  {w}×{h}"))))
            .collect();
        let (size, count) = (self.size, self.sizes.len() + 1);
        row().key("playground-size").h(CONTROL).pl(8.0).pr(4.0).gap(2.0).center_y().radius(radius::SM)
            .hover(t.hover).press(t.press)
            .child(text(current).font(type_::LABEL).color(t.text))
            .child(icon(&CHEVRON_DOWN, 16.0).color(t.text_secondary))
            .role("button").label("Stage size")
            .on_press_tx(move |_: &mut Controls, tx: &mut Tx| {
                if !crate::popup_menu_supported() {
                    tx.playground.push(Request::Size((size + 1) % count));
                    return;
                }
                let menu = crate::PopupMenu::picker(options.clone(), &size.to_string());
                tx.popup_menu(menu, "playground-size", |_: &mut Controls, picked, tx| {
                    if let Some(i) = picked.and_then(|id| id.parse().ok()) { tx.playground.push(Request::Size(i)); }
                });
            })
    }

    /// The column right of the stage: the story's group and name, the app's controls for it,
    /// its triggers, and a key hint.
    fn side(&self, app: &S, cx: &mut Cx<S>, t: Theme, dark: bool, height: f32) -> El {
        let i = self.selected;
        let title = (i < self.count()).then(|| {
            col().key("playground-title").px(4.0).gap(2.0)
                .child(text(self.group(i)).font(type_::SECTION).color(t.text_secondary))
                .child(text(self.name(i)).font(type_::TITLE).color(t.text).truncate())
        });
        // Keyed by story, not generation, so a replay keeps the keyboard on the control it's on.
        let slot = self.side_slot(app, cx).map(|slot| el().key(format!("playground-slot-{i}")).w_full().child(slot));
        let variants = self.variants();
        let at = self.variant_at();
        let picker = (!variants.is_empty()).then(|| {
            col().key("playground-variants").w_full().padding(4.0).gap(2.0).radius(radius::LG).fill(t.card).outline(1.0, 0.0, t.card_border)
                .children(variants.iter().enumerate().take(9).map(|(n, &name)| {
                    let on = n == at;
                    row().key(format!("playground-variant-{n}")).w_full().h(ROW).px(4.0).gap(8.0).center_y().radius(radius::MD)
                        .fill(if on { selected(t, dark) } else { Color::TRANSPARENT })
                        .focusable(true).focus_highlight(selected(t, dark)).press(t.press)
                        .child(keycap(t, &(n + 1).to_string()))
                        .child(text(name).font(type_::BODY).color(if on { t.text } else { t.text_secondary }).truncate())
                        .role("button").label(name)
                        .on_press_tx(move |_: &mut Controls, tx: &mut Tx| tx.playground.push(Request::Variant(n)))
                }))
        });
        let triggers = self.triggers();
        let card = (!triggers.is_empty()).then(|| {
            col().key("playground-triggers").w_full().padding(4.0).gap(2.0).radius(radius::LG).fill(t.card).outline(1.0, 0.0, t.card_border)
                .children(triggers.iter().map(|&(key, label)| {
                    let request = if key == 'r' { Request::Replay } else { Request::Trigger(key) };
                    row().key(format!("playground-trigger-{key}")).w_full().h(ROW).px(4.0).gap(8.0).center_y().radius(radius::MD)
                        .focusable(true).focus_highlight(selected(t, dark)).press(t.press)
                        .child(keycap(t, &cap(key)))
                        .child(text(label).font(type_::BODY).color(t.text).truncate())
                        .role("button").label(label)
                        .on_press_tx(move |_: &mut Controls, tx: &mut Tx| tx.playground.push(request.clone()))
                }))
        });
        let hint = if triggers.is_empty() { "↑↓ stories · Esc back" } else { "↑↓ stories · → triggers · Esc back" };
        col().key("playground-side").w(SIDE).h(height).px(12.0).pt(12.0).pb(12.0).gap(12.0)
            .children(title)
            .children(slot)
            .children(picker)
            .children(card)
            .child(spacer())
            .child(text(hint).key("playground-hint").font(type_::SECTION).color(t.text_secondary).px(4.0))
    }

    /// The app's controls for the open story, from `Playground::side` or `Story::side`, acting
    /// on its stage state.
    fn side_slot(&self, app: &S, cx: &mut Cx<S>) -> Option<El> {
        let _ = app;
        if let Some(scene) = &self.scene {
            let (_, view) = self.sides.iter().find(|(s, _)| *s == self.selected)?;
            let mut stage = cx.retype::<S>();
            let root = call(*view, &scene.state, &mut stage);
            cx.absorb(stage);
            return Some(root);
        }
        let (own, stage) = self.own()?;
        let mut unit = cx.retype::<()>();
        let root = own.side(stage, &mut unit);
        cx.absorb(unit);
        root
    }

    fn stage(&self, app: &S, view: fn(&S, &mut Cx<S>) -> El, cx: &mut Cx<S>, area: (f32, f32), t: Theme, dark: bool) -> El {
        let mut env = cx.env();
        env.system_dark = self.dark.unwrap_or(dark);
        let page = self.page();
        let frame = self.own().and_then(|(own, _)| own.frame());
        let preset = self.size.checked_sub(1).and_then(|i| self.sizes.get(i)).map(|(_, size)| *size);
        let (w, h) = frame.or(preset).unwrap_or(area);
        let content = match (&self.scene, page) {
            (Some(scene), _) => Some(self.nested(cx, env, |c| call(view, &scene.state, c))),
            (None, Some((_, Page { kind: Kind::Live(page), .. }))) => Some(self.nested(cx, env, |c| call(*page, app, c))),
            (None, Some(_)) => self.own().map(|(own, stage)| {
                let mut unit = cx.retype::<()>();
                unit.set_env(env);
                unit.set_titlebar_inset(0.0);
                if self.reduce_motion { unit.reduced_motion(true); }
                let root = own.view(stage, &mut unit);
                cx.absorb(unit);
                root
            }),
            (None, None) => None,
        };
        let inner = tok::theme(env.system_dark);
        let content = content.unwrap_or_else(|| {
            col().center().child(text("Add scenes to AppSpec::scenes, or stories to the playground").font(type_::BODY).color(inner.text_secondary))
        });
        // Sized as a window sizes its root.
        let stage = el().key(format!("playground-stage-{}", self.generation)).w(w).h(h).child(content.w(w).h(h));
        let stage = match (frame, preset) {
            // An overlay's clear parts stay clear.
            (Some(_), _) => stage.border(1.0, t.text_secondary.alpha(0.35)).border_dash(4.0, 3.0),
            (None, Some(_)) => stage.outline(1.0, 0.0, inner.separator),
            (None, None) => stage,
        };
        col().w(area.0).h(area.1).center().fill(inner.bg).clip_rounded(0.0).child(stage)
    }

    /// Runs a stage view of the app's own type with the stage's appearance and motion.
    fn nested(&self, cx: &mut Cx<S>, env: crate::Env, f: impl FnOnce(&mut Cx<S>) -> El) -> El {
        let mut stage = cx.retype::<S>();
        stage.set_env(env);
        stage.set_titlebar_inset(0.0);
        if self.reduce_motion { stage.reduced_motion(true); }
        let root = f(&mut stage);
        cx.absorb(stage);
        root
    }
}

/// A selected row's fill, a step stronger in dark mode where the track is faint.
fn selected(t: Theme, dark: bool) -> Color {
    if dark { t.control_off } else { t.control_track }
}

/// A key's cap beside its control in the side column.
fn keycap(t: Theme, key: &str) -> El {
    row().key("cap").h(20.0).min_w(22.0).px(6.0).center().radius(radius::SM - 2.0).fill(t.control_track)
        .child(text(key.to_owned()).font(type_::LABEL).color(t.text))
}

/// A trigger's key as its cap shows it.
fn cap(key: char) -> String {
    if key == ' ' { "Space".into() } else { key.to_ascii_uppercase().to_string() }
}

/// A shortcut as the bar shows it: "⇧⌘D", or "Ctrl+Shift+D" off macOS.
fn keycaps(shortcut: &Shortcut) -> String {
    shortcut.keycaps().join(if cfg!(target_os = "macos") { "" } else { "+" })
}

/// A quiet square button showing `glyph`, named by `label` in its tooltip; `on` fills it as the
/// list's selection is filled.
fn icon_button(t: Theme, key: &str, glyph: &'static Icon, label: &str, on: bool, dark: bool, request: Request) -> El {
    row().key(key.to_owned()).size(CONTROL, CONTROL).center().radius(radius::SM)
        .fill(if on { selected(t, dark) } else { Color::TRANSPARENT })
        .hover(if on { selected(t, dark) } else { t.hover }).press(t.press)
        .child(icon(glyph, 16.0).color(if on { t.text } else { t.text_secondary }))
        .role("button").label(label.to_owned()).help(label.to_owned())
        .on_press_tx(move |_: &mut Controls, tx: &mut Tx| tx.playground.push(request.clone()))
}

static CHEVRON_LEFT: Icon = Icon {
    name: "playground-chevron-left",
    view: 16.0,
    body: r#"<path d="M9.5 4L5.5 8L9.5 12" stroke="currentColor" stroke-width="1.5" fill="none" stroke-linecap="round" stroke-linejoin="round"/>"#,
};

static GRID: Icon = Icon {
    name: "playground-grid",
    view: 12.0,
    body: r#"<rect x="1" y="1" width="4" height="4" rx="1" fill="currentColor"/><rect x="7" y="1" width="4" height="4" rx="1" fill="currentColor"/><rect x="1" y="7" width="4" height="4" rx="1" fill="currentColor"/><rect x="7" y="7" width="4" height="4" rx="1" fill="currentColor"/>"#,
};

static CHEVRON_RIGHT: Icon = Icon {
    name: "playground-chevron-right",
    view: 16.0,
    body: r#"<path d="M6.75 5L9.75 8L6.75 11" stroke="currentColor" stroke-width="1.5" fill="none" stroke-linecap="round" stroke-linejoin="round"/>"#,
};

static CHEVRON_DOWN: Icon = Icon {
    name: "playground-chevron-down",
    view: 16.0,
    body: r#"<path d="M5 6.75L8 9.75L11 6.75" stroke="currentColor" stroke-width="1.5" fill="none" stroke-linecap="round" stroke-linejoin="round"/>"#,
};

static SUN: Icon = Icon {
    name: "playground-sun",
    view: 16.0,
    body: r#"<circle cx="8" cy="8" r="2.75" stroke="currentColor" stroke-width="1.5" fill="none"/><path d="M8 1.75V3M8 13V14.25M1.75 8H3M13 8H14.25M3.6 3.6L4.45 4.45M11.55 11.55L12.4 12.4M3.6 12.4L4.45 11.55M11.55 4.45L12.4 3.6" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>"#,
};

static MOON: Icon = Icon {
    name: "playground-moon",
    view: 16.0,
    body: r#"<path d="M13.25 9.5A5.5 5.5 0 0 1 6.5 2.75A5.5 5.5 0 1 0 13.25 9.5Z" stroke="currentColor" stroke-width="1.5" fill="none" stroke-linejoin="round"/>"#,
};

static MOTION: Icon = Icon {
    name: "playground-motion",
    view: 16.0,
    body: r#"<circle cx="10" cy="8" r="3.25" stroke="currentColor" stroke-width="1.5" fill="none"/><path d="M1.75 5.5H4.5M1.25 8H4M1.75 10.5H4.5" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>"#,
};
