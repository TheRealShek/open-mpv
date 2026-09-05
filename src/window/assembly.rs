//! Constructs the application widgets, static menus, styling, and initial signal wiring.

use super::*;

impl App {
    pub fn new(gtk_app: &gtk::Application, cfg: Config) -> Rc<App> {
        let win = gtk::ApplicationWindow::builder()
            .application(gtk_app)
            .title("open-mpv")
            .decorated(false)
            .build();

        apply_css(&cfg.background);

        let view = ImageView::default();
        view.set_default_fit_actual(cfg.fit == FitMode::Actual);

        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&view));

        // Empty / error state (FR-1.4/1.5). These are real buttons rather
        // than a clickable label, so both choices have distinct accessible
        // names and normal keyboard focus.
        let status = gtk::Label::new(Some("Open a file or folder…"));
        status.set_wrap(true);
        status.set_justify(gtk::Justification::Center);
        status.add_css_class("status");
        let open_file_btn = gtk::Button::with_label("Open File…");
        open_file_btn.set_action_name(Some(&Action::OpenFile.detailed_name()));
        let open_folder_btn = gtk::Button::with_label("Open Folder…");
        open_folder_btn.set_action_name(Some(&Action::OpenFolder.detailed_name()));
        let status_actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        status_actions.set_halign(gtk::Align::Center);
        status_actions.add_css_class("status-actions");
        status_actions.append(&open_file_btn);
        status_actions.append(&open_folder_btn);
        let status_area = gtk::Box::new(gtk::Orientation::Vertical, 12);
        status_area.set_halign(gtk::Align::Center);
        status_area.set_valign(gtk::Align::Center);
        status_area.append(&status);
        status_area.append(&status_actions);
        overlay.add_overlay(&status_area);

        // Navigation arrows (FR-3.1).
        let prev_btn = osd_button(
            "go-previous-symbolic",
            &Action::Previous.detailed_name(),
            "Previous image",
        );
        prev_btn.set_halign(gtk::Align::Start);
        prev_btn.set_valign(gtk::Align::Center);
        overlay.add_overlay(&prev_btn);
        let next_btn = osd_button(
            "go-next-symbolic",
            &Action::Next.detailed_name(),
            "Next image",
        );
        next_btn.set_halign(gtk::Align::End);
        next_btn.set_valign(gtk::Align::Center);
        overlay.add_overlay(&next_btn);

        // Information belongs apart from actions: the filename and folder
        // position form a quiet top-left pill (FR-3.4, FR-6.2).
        let info_bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        info_bar.add_css_class("osd-surface");
        info_bar.add_css_class("info-bar");
        info_bar.set_halign(gtk::Align::Start);
        info_bar.set_valign(gtk::Align::Start);
        info_bar.set_visible(false);
        let name_label = gtk::Label::new(None);
        name_label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        name_label.set_max_width_chars(28);
        info_bar.append(&name_label);
        let pos_label = gtk::Label::new(None);
        pos_label.add_css_class("position");
        info_bar.append(&pos_label);
        overlay.add_overlay(&info_bar);

        // Window controls stay together in the opposite corner rather
        // than competing with media actions in the bottom bar (FR-6.3/6.7).
        let window_controls = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        window_controls.add_css_class("osd-surface");
        window_controls.add_css_class("window-controls");
        window_controls.set_halign(gtk::Align::End);
        window_controls.set_valign(gtk::Align::Start);
        let fullscreen_btn = bar_button(
            "view-fullscreen-symbolic",
            &Action::Fullscreen.detailed_name(),
            "Fullscreen",
        );
        window_controls.append(&fullscreen_btn);
        // Close button (FR-6.7).
        // Ask for the regular icon: some third-party themes ship a white
        // `-symbolic` source that GTK recolours to transparent.
        let close_btn = bar_button("window-close", &Action::Close.detailed_name(), "Close");
        window_controls.append(&close_btn);
        overlay.add_overlay(&window_controls);

        // Bottom media controls (FR-6.2). Photo and video actions occupy
        // the same place but never appear together.
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        bar.add_css_class("osd-surface");
        bar.add_css_class("osd-bar");
        bar.set_halign(gtk::Align::Center);
        bar.set_valign(gtk::Align::End);
        let normal_controls = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        bar.append(&normal_controls);
        // Video transport: seek bar + position, hidden for images
        // (FR-10.5). The position tick runs only while this is visible.
        let seek_bar = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.01);
        seek_bar.set_size_request(SEEK_BAR_MAX_WIDTH, -1);
        seek_bar.set_draw_value(false);
        // `with_range` derives round-digits from the step increment, which
        // would quantise the thumb to 1 % of the duration — 36 s of a
        // one-hour video. Scrubbing has to be continuous.
        seek_bar.set_round_digits(-1);
        let time_label = gtk::Label::new(None);
        time_label.add_css_class("dim");
        // Reserve "0:00 / 0:00" so the bar does not twitch every time a
        // digit rolls over; longer runtimes grow it once and settle.
        time_label.set_width_chars(11);
        // A seek bar with no play button was the clearest hole in the
        // overlay: pausing was keyboard-only (FR-6.5). Icons track the
        // pipeline in update_transport.
        let play_btn = bar_button(
            "media-playback-pause-symbolic",
            &Action::PlayPause.detailed_name(),
            "Play / pause",
        );
        let mute_btn = bar_button(
            "audio-volume-high-symbolic",
            &Action::Mute.detailed_name(),
            "Mute",
        );
        let speed_menu = playback_speed_menu();
        let speed_label = gtk::Label::new(Some("1×"));
        let speed_btn = gtk::MenuButton::builder()
            .child(&speed_label)
            .menu_model(&speed_menu)
            .tooltip_text("Playback speed")
            .build();
        speed_btn.add_css_class("flat");
        let subtitle_menu = gio::Menu::new();
        let subtitle_btn = gtk::MenuButton::builder()
            .icon_name("media-view-subtitles-symbolic")
            .menu_model(&subtitle_menu)
            .tooltip_text("Subtitles")
            .build();
        subtitle_btn.add_css_class("flat");
        subtitle_btn.set_visible(false);
        let transport = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        transport.append(&play_btn);
        transport.append(&seek_bar);
        transport.append(&time_label);
        transport.append(&speed_btn);
        transport.append(&subtitle_btn);
        transport.append(&mute_btn);
        transport.set_visible(false);
        normal_controls.append(&transport);

        let photo_controls = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let rotate_ccw = bar_button(
            "object-rotate-left-symbolic",
            &Action::RotateCounterclockwise.detailed_name(),
            "Rotate left",
        );
        let rotate_cw = bar_button(
            "object-rotate-right-symbolic",
            &Action::RotateClockwise.detailed_name(),
            "Rotate right",
        );
        photo_controls.append(&rotate_ccw);
        photo_controls.append(&rotate_cw);
        let markup_btn = bar_button(
            "document-edit-symbolic",
            &Action::Markup.detailed_name(),
            "Quick Markup",
        );
        photo_controls.append(&markup_btn);
        // Save appears only once an editable image has a pending rotation;
        // an inert floppy icon reads as a broken control (FR-5.4).
        let save_btn = bar_button(
            "document-save-symbolic",
            &Action::Save.detailed_name(),
            "Save rotation to file",
        );
        save_btn.set_visible(false);
        photo_controls.append(&save_btn);
        photo_controls.set_visible(false);
        normal_controls.append(&photo_controls);

        let separator = gtk::Separator::new(gtk::Orientation::Vertical);
        normal_controls.append(&separator);
        normal_controls.append(&bar_button(
            "user-trash-symbolic",
            &Action::Trash.detailed_name(),
            "Move to trash",
        ));

        // Less frequent commands remain discoverable without making the
        // primary strip permanent or wide (FR-6.5, NFR-5.2).
        let more_menu = gio::Menu::new();
        let open_menu = open_menu_model();
        more_menu.append_section(None, &open_menu);
        more_menu.append(
            Some("Fit to Window"),
            Some(&Action::ZoomFit.detailed_name()),
        );
        more_menu.append(
            Some("Actual Size"),
            Some(&Action::ZoomActual.detailed_name()),
        );
        more_menu.append(
            Some("Rotate Left"),
            Some(&Action::RotateCounterclockwise.detailed_name()),
        );
        more_menu.append(
            Some("Rotate Right"),
            Some(&Action::RotateClockwise.detailed_name()),
        );
        more_menu.append(Some("First File"), Some(&Action::First.detailed_name()));
        more_menu.append(Some("Last File"), Some(&Action::Last.detailed_name()));
        // Filled only for decoded static raster images (FR-11.1). A disabled
        // editing item on video or animation would imply future support.
        let markup_context_menu = gio::Menu::new();
        more_menu.append_section(None, &markup_context_menu);
        // Audio-track selection is contextual and appears only when the
        // current video exposes multiple tracks (FR-10.8).
        let audio_menu = gio::Menu::new();
        let audio_context_menu = gio::Menu::new();
        more_menu.append_section(None, &audio_context_menu);
        // Filled only while a video is active. The same subtitle model is
        // shared with the CC button, so right-click and the transport never
        // disagree about available tracks (FR-10.7).
        let subtitle_context_menu = gio::Menu::new();
        more_menu.append_section(None, &subtitle_context_menu);
        more_menu.append(
            Some("Keyboard Shortcuts"),
            Some(&Action::Help.detailed_name()),
        );
        let more_btn = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .menu_model(&more_menu)
            .tooltip_text("More controls")
            .build();
        more_btn.add_css_class("flat");
        normal_controls.append(&more_btn);

        // Quick Markup is a focused mode: replace the normal controls so
        // navigation and file operations are not sitting beside a drawing
        // gesture that temporarily changes what primary drag means.
        let markup_controls = gtk::Box::new(gtk::Orientation::Vertical, 4);
        markup_controls.add_css_class("markup-controls");
        let markup_tools = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        markup_tools.set_halign(gtk::Align::Center);
        let markup_decisions = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        markup_decisions.set_halign(gtk::Align::Center);
        let markup_box_btn = gtk::ToggleButton::with_label("Box");
        markup_box_btn.set_action_name(Some(&Action::MarkupBox.detailed_name()));
        markup_box_btn.set_tooltip_text(Some("Draw a box (B)"));
        markup_box_btn.add_css_class("flat");
        markup_box_btn.set_active(true);
        let markup_arrow_btn = gtk::ToggleButton::with_label("Arrow");
        markup_arrow_btn.set_group(Some(&markup_box_btn));
        markup_arrow_btn.set_action_name(Some(&Action::MarkupArrow.detailed_name()));
        markup_arrow_btn.set_tooltip_text(Some("Draw an arrow (Shift+A)"));
        markup_arrow_btn.add_css_class("flat");
        markup_tools.append(&markup_box_btn);
        markup_tools.append(&markup_arrow_btn);
        markup_tools.append(&bar_button(
            "edit-undo-symbolic",
            &Action::Undo.detailed_name(),
            "Undo last annotation (Ctrl+Z)",
        ));
        markup_decisions.append(&bar_button(
            "edit-clear-all-symbolic",
            &Action::MarkupClear.detailed_name(),
            "Clear all annotations; Ctrl+Z restores them (C)",
        ));
        markup_decisions.append(&bar_button(
            "process-stop-symbolic",
            &Action::Markup.detailed_name(),
            "Cancel Quick Markup (A or Escape)",
        ));
        let copy_markup_btn = bar_button(
            "edit-copy-symbolic",
            &Action::MarkupCopy.detailed_name(),
            "Copy annotated image (Ctrl+C)",
        );
        copy_markup_btn.remove_css_class("flat");
        copy_markup_btn.add_css_class("suggested-action");
        markup_decisions.append(&copy_markup_btn);
        markup_controls.append(&markup_tools);
        markup_controls.append(&markup_decisions);
        markup_controls.set_visible(false);
        bar.append(&markup_controls);

        // The same commands are also a contextual menu on the medium.
        // It is parented to the view so its pointing rectangle uses the
        // secondary-click coordinates directly.
        let context_menu = gtk::PopoverMenu::from_model(Some(&more_menu));
        context_menu.set_parent(&view);
        bar.set_visible(false);
        overlay.add_overlay(&bar);

        // Zoom / edge-cue indicator (FR-4.4, FR-3.3).
        let indicator = gtk::Label::new(None);
        indicator.add_css_class("indicator");
        indicator.add_css_class("invisible");
        indicator.set_halign(gtk::Align::Center);
        indicator.set_valign(gtk::Align::Start);
        indicator.set_can_target(false);
        overlay.add_overlay(&indicator);

        // Toast with undo (FR-5.2).
        let toast_label = gtk::Label::new(None);
        let toast_undo = gtk::Button::with_label("Undo");
        toast_undo.set_action_name(Some(&Action::Undo.detailed_name()));
        let toast_box = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        toast_box.add_css_class("toast");
        toast_box.append(&toast_label);
        toast_box.append(&toast_undo);
        let toast_revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::Crossfade)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::End)
            .margin_bottom(56)
            .child(&toast_box)
            .build();
        overlay.add_overlay(&toast_revealer);

        // Keybinding cheat sheet (NFR-5.2).
        let help_label = gtk::Label::new(None);
        help_label.add_css_class("help");
        help_label.set_halign(gtk::Align::Center);
        help_label.set_valign(gtk::Align::Center);
        help_label.set_visible(false);
        overlay.add_overlay(&help_label);

        win.set_child(Some(&overlay));

        for w in [
            prev_btn.upcast_ref::<gtk::Widget>(),
            next_btn.upcast_ref(),
            info_bar.upcast_ref(),
            window_controls.upcast_ref(),
            bar.upcast_ref(),
        ] {
            w.add_css_class("chrome");
            w.add_css_class("invisible");
            w.set_can_target(false);
        }

        let subtitle_action = gio::SimpleAction::new_stateful(
            Action::SubtitleSelect.name(),
            Some(glib::VariantTy::STRING),
            &"auto".to_variant(),
        );
        let audio_action = gio::SimpleAction::new_stateful(
            Action::AudioSelect.name(),
            Some(glib::VariantTy::STRING),
            &"auto".to_variant(),
        );
        let speed_action = gio::SimpleAction::new_stateful(
            Action::SpeedSet.name(),
            Some(glib::VariantTy::DOUBLE),
            &1.0_f64.to_variant(),
        );
        let app = Rc::new(App {
            win: win.clone(),
            view: view.clone(),
            navigation: RefCell::new(Navigation::default()),
            monitor: RefCell::new(None),
            fs_queries: RefCell::new(FsQueryVersions::default()),
            media: RefCell::new(MediaState::Empty),
            cache: loader::Cache::new(3, cache_budget_bytes(cfg.cache_budget_mb)),
            editable_mimes: RefCell::new(BTreeSet::new()),
            player: RefCell::new(None),
            operations: RefCell::new(OperationCoordinator::default()),
            presented: Cell::new(false),
            status_area,
            status,
            info_bar: info_bar.clone(),
            name_label,
            pos_label,
            prev_btn: prev_btn.clone(),
            next_btn: next_btn.clone(),
            normal_controls,
            photo_controls,
            markup_btn,
            markup_controls,
            markup_box_btn,
            markup_arrow_btn,
            transport,
            play_btn,
            mute_btn,
            speed_btn: speed_btn.clone(),
            speed_label,
            speed_action,
            subtitle_btn: subtitle_btn.clone(),
            markup_context_menu,
            audio_menu,
            audio_context_menu,
            audio_action,
            subtitle_menu,
            subtitle_context_menu,
            subtitle_action,
            save_btn,
            fitted_for: Cell::new(None),
            control_bar: bar.clone(),
            seek_bar,
            time_label,
            transport_tick: RefCell::new(None),
            scrubbing: Cell::new(false),
            pointer_on_chrome: Cell::new(false),
            pointer_on_status: Cell::new(false),
            menu_open: Cell::new(false),
            pointer: Cell::new((0.0, 0.0)),
            inhibit_cookie: Cell::new(None),
            shutting_down: Cell::new(false),
            sized_from_media: Cell::new(false),
            indicator,
            toast_revealer,
            toast_label,
            toast_undo,
            toast_undo_id: Cell::new(None),
            help_label,
            chrome: vec![
                prev_btn.upcast(),
                next_btn.upcast(),
                info_bar.upcast(),
                window_controls.upcast(),
                bar.upcast(),
            ],
            chrome_timer: TimerSlot::default(),
            indicator_timer: TimerSlot::default(),
            toast_timer: TimerSlot::default(),
            undo_timer: TimerSlot::default(),
            svg_timer: TimerSlot::default(),
            markup_action: gio::SimpleAction::new(Action::Markup.name(), None),
            markup_box_action: gio::SimpleAction::new(Action::MarkupBox.name(), None),
            markup_arrow_action: gio::SimpleAction::new(Action::MarkupArrow.name(), None),
            markup_copy_action: gio::SimpleAction::new(Action::MarkupCopy.name(), None),
            markup_clear_action: gio::SimpleAction::new(Action::MarkupClear.name(), None),
            save_action: gio::SimpleAction::new(Action::Save.name(), None),
            undo_action: gio::SimpleAction::new(Action::Undo.name(), None),
            cfg,
        });

        app.setup_actions(gtk_app);
        app.setup_controllers();
        app.build_help(gtk_app);

        // Every close route eventually emits close-request, including the
        // window manager and the typed close/escape actions. Tear playback
        // down while GTK's main loop and GStreamer libraries are still fully
        // alive; waiting for `Player::drop` can race process-wide destructors.
        app.win.connect_close_request(clone!(
            #[strong(rename_to = app)]
            app,
            move |_| {
                app.shutdown();
                glib::Propagation::Proceed
            }
        ));

        more_btn.connect_active_notify(clone!(
            #[strong(rename_to = app)]
            app,
            move |button| {
                app.menu_open.set(button.is_active());
                // Opening can move the pointer onto the popover surface
                // before GTK reports it active, briefly firing the window
                // leave handler. Reveal again after that race; while open,
                // menu_open blocks every later fade. Closing starts a fresh
                // timeout in the same call.
                app.show_chrome();
            }
        ));
        subtitle_btn.connect_active_notify(clone!(
            #[strong(rename_to = app)]
            app,
            move |button| {
                app.menu_open.set(button.is_active());
                app.show_chrome();
            }
        ));
        speed_btn.connect_active_notify(clone!(
            #[strong(rename_to = app)]
            app,
            move |button| {
                app.menu_open.set(button.is_active());
                app.show_chrome();
            }
        ));
        context_menu.connect_visible_notify(clone!(
            #[strong(rename_to = app)]
            app,
            move |menu| {
                app.menu_open.set(menu.is_visible());
                app.show_chrome();
            }
        ));

        let context_click = gtk::GestureClick::new();
        context_click.set_button(gdk::BUTTON_SECONDARY);
        context_click.connect_pressed(clone!(
            #[strong(rename_to = app)]
            app,
            #[weak]
            context_menu,
            move |gesture, _, x, y| {
                let showing_media = matches!(
                    *app.media.borrow(),
                    MediaState::Image { .. } | MediaState::Video(_)
                );
                if !showing_media || app.view.is_marking_up() {
                    return;
                }
                gesture.set_state(gtk::EventSequenceState::Claimed);
                context_menu.set_pointing_to(Some(&gdk::Rectangle::new(
                    window_dimension(x, 0),
                    window_dimension(y, 0),
                    1,
                    1,
                )));
                context_menu.popup();
            }
        ));
        view.add_controller(context_click);

        if app.cfg.start_fullscreen {
            app.win.fullscreen();
        }

        // Clicking or dragging the seek bar seeks. Proceed — not Stop —
        // because GtkRange only moves the thumb to the pointer in its
        // default handler, which a handled signal would suppress. There is
        // no feedback loop: programmatic set_value does not fire
        // change-value.
        app.seek_bar.connect_change_value(clone!(
            #[strong(rename_to = app)]
            app,
            move |_, _, value| {
                app.with_video(|p| p.seek_fraction(value));
                glib::Propagation::Proceed
            }
        ));

        // Knowing when the pointer holds the bar keeps playback from
        // dragging the thumb back mid-scrub. GtkRange claims the pointer
        // sequence for its own drag, which would cancel a GestureClick of
        // ours — watching the raw events is what survives that.
        let press = gtk::EventControllerLegacy::new();
        press.set_propagation_phase(gtk::PropagationPhase::Capture);
        press.connect_event(clone!(
            #[strong(rename_to = app)]
            app,
            move |_, event| {
                match event.event_type() {
                    gdk::EventType::ButtonPress | gdk::EventType::TouchBegin => {
                        app.scrubbing.set(true)
                    }
                    gdk::EventType::ButtonRelease
                    | gdk::EventType::TouchEnd
                    | gdk::EventType::TouchCancel => {
                        app.scrubbing.set(false);
                        // Restart the fade-out that was held off while the
                        // bar was under the pointer.
                        app.show_chrome();
                    }
                    _ => {}
                }
                glib::Propagation::Proceed
            }
        ));
        app.seek_bar.add_controller(press);

        // Query which formats the sandboxed editors can rewrite.
        glib::spawn_future_local(clone!(
            #[strong]
            app,
            async move {
                let mimes = fileops::editable_mime_types().await;
                *app.editable_mimes.borrow_mut() = mimes;
                app.update_save_enabled();
            }
        ));

        app.view.connect_view_changed(clone!(
            #[strong]
            app,
            move |zoom_percent| {
                app.flash(&format!("{zoom_percent:.0}%"));
                app.update_save_enabled();
                app.schedule_svg_rerender();
            }
        ));
        app.view.connect_annotation_changed(clone!(
            #[strong]
            app,
            move |status| app.on_markup_changed(status)
        ));
        app.view.connect_navigate(clone!(
            #[strong]
            app,
            move |dir| {
                app.dispatch_action(if dir > 0 {
                    Action::Next
                } else {
                    Action::Previous
                });
            }
        ));
        // A video is on screen before the pipeline knows how big it is;
        // the size arrives with preroll (FR-6.6).
        app.view.connect_source_size(clone!(
            #[strong]
            app,
            move |size| app.size_to_media(size)
        ));

        app
    }

    fn setup_actions(self: &Rc<Self>, gtk_app: &gtk::Application) {
        const MANAGED_ACTIONS: &[Action] = &[
            Action::Markup,
            Action::MarkupBox,
            Action::MarkupArrow,
            Action::MarkupCopy,
            Action::MarkupClear,
            Action::Save,
            Action::Undo,
        ];
        for (name, _) in Action::CONFIGURABLE {
            if MANAGED_ACTIONS.contains(name) {
                continue;
            }
            let action = gio::SimpleAction::new(name.name(), None);
            let app = self.clone();
            let name = *name;
            action.connect_activate(move |_, _| app.dispatch_action(name));
            self.win.add_action(&action);
        }

        let app = self.clone();
        self.markup_action
            .connect_activate(move |_, _| app.dispatch_action(Action::Markup));
        self.win.add_action(&self.markup_action);

        let app = self.clone();
        self.markup_box_action.connect_activate(move |_, _| {
            app.dispatch_action(Action::MarkupBox);
        });
        self.win.add_action(&self.markup_box_action);

        let app = self.clone();
        self.markup_arrow_action.connect_activate(move |_, _| {
            app.dispatch_action(Action::MarkupArrow);
        });
        self.win.add_action(&self.markup_arrow_action);

        let app = self.clone();
        self.markup_copy_action
            .connect_activate(move |_, _| app.dispatch_action(Action::MarkupCopy));
        self.win.add_action(&self.markup_copy_action);

        let app = self.clone();
        self.markup_clear_action.connect_activate(move |_, _| {
            app.dispatch_action(Action::MarkupClear);
        });
        self.win.add_action(&self.markup_clear_action);

        let app = self.clone();
        self.save_action.set_enabled(false);
        self.save_action
            .connect_activate(move |_, _| app.dispatch_action(Action::Save));
        self.win.add_action(&self.save_action);

        let app = self.clone();
        self.undo_action.set_enabled(false);
        self.undo_action.connect_activate(move |_, _| {
            app.dispatch_action(Action::Undo);
        });
        self.win.add_action(&self.undo_action);

        let app = self.clone();
        self.subtitle_action.connect_activate(move |_, parameter| {
            let Some(target) = parameter.and_then(glib::Variant::str) else {
                return;
            };
            let choice = match target {
                "auto" => SubtitleChoice::Automatic,
                "off" => SubtitleChoice::Off,
                target => {
                    let Some(id) = target.strip_prefix("track:") else {
                        return;
                    };
                    SubtitleChoice::Track(id.to_string())
                }
            };
            app.dispatch_subtitle(choice);
        });
        self.win.add_action(&self.subtitle_action);

        let app = self.clone();
        self.audio_action.connect_activate(move |_, parameter| {
            let Some(target) = parameter.and_then(glib::Variant::str) else {
                return;
            };
            let choice = match target {
                "auto" => AudioChoice::Automatic,
                target => {
                    let Some(id) = target.strip_prefix("track:") else {
                        return;
                    };
                    AudioChoice::Track(id.to_string())
                }
            };
            app.dispatch_audio(choice);
        });
        self.win.add_action(&self.audio_action);

        let app = self.clone();
        self.speed_action.connect_activate(move |_, parameter| {
            let Some(rate) = parameter.and_then(glib::Variant::get::<f64>) else {
                return;
            };
            app.dispatch_speed(rate);
        });
        self.win.add_action(&self.speed_action);

        // Defaults merged with user binds (FR-8.2); a user bind takes
        // the key over from the default action.
        let mut key_to_action: BTreeMap<String, Action> = Action::DEFAULT_BINDS
            .iter()
            .map(|(key, action)| (key.to_string(), *action))
            .collect();
        for (key, action) in &self.cfg.binds {
            if gtk::accelerator_parse(key).is_none() {
                eprintln!("open-mpv: config: cannot parse key `{key}`");
                continue;
            }
            // `bind=<key> none` takes a key away without replacing it —
            // otherwise a default binding can only ever be overridden.
            if action == "none" {
                key_to_action.remove(key);
                continue;
            }
            let Some(action) = Action::parse(action) else {
                eprintln!("open-mpv: config: unknown action `{action}` for bind `{key}`");
                continue;
            };
            key_to_action.insert(key.clone(), action);
        }
        let mut action_to_keys: BTreeMap<Action, Vec<&str>> = BTreeMap::new();
        for (key, action) in &key_to_action {
            action_to_keys
                .entry(*action)
                .or_default()
                .push(key.as_str());
        }
        for (action, keys) in &action_to_keys {
            gtk_app.set_accels_for_action(&action.detailed_name(), keys);
        }
        self.update_control_mode();
    }

    fn build_help(&self, gtk_app: &gtk::Application) {
        let mut rows: Vec<(String, &str)> = Vec::new();
        for (action, description) in Action::CONFIGURABLE {
            if description.is_empty() {
                continue;
            }
            let accels = gtk_app.accels_for_action(&action.detailed_name());
            if accels.is_empty() {
                continue;
            }
            // "<Control>z" is the binding syntax, not something to read;
            // GTK renders it the way the rest of the desktop writes it.
            let keys: Vec<String> = accels
                .iter()
                .map(|a| match gtk::accelerator_parse(a) {
                    Some((key, mods)) => gtk::accelerator_get_label(key, mods).to_string(),
                    None => a.to_string(),
                })
                .collect();
            rows.push((keys.join(", "), description));
        }
        let width = rows
            .iter()
            .map(|(keys, _)| keys.chars().count())
            .max()
            .unwrap_or(0);
        let mut lines = vec!["<b>Keys</b>".to_string(), String::new()];
        lines.extend(
            rows.iter()
                .map(|(keys, description)| help_line(keys, description, width)),
        );
        lines.push(String::new());
        lines.push(
            "<tt>Escape</tt> cancels Quick Markup, leaves fullscreen, then closes".to_string(),
        );
        lines.push("Scroll: zoom · horizontal scroll: navigate".to_string());
        lines.push("Drag: pan (zoomed) or move window · double-click: fullscreen".to_string());
        lines.push("Drag an edge or corner: resize the window".to_string());
        self.help_label.set_markup(&lines.join("\n"));
    }

    fn setup_controllers(self: &Rc<Self>) {
        // Mouse movement reveals the chrome (FR-6.2); keyboard-only use
        // never shows it.
        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(clone!(
            #[strong(rename_to = app)]
            self,
            move |_, x, y| {
                app.pointer.set((x, y));
                app.show_chrome();
                app.update_cursor();
            }
        ));
        motion.connect_leave(clone!(
            #[strong(rename_to = app)]
            self,
            move |_| {
                // Order matters: hide_chrome re-decides the cursor from
                // the last known position, so the reset goes after it.
                app.hide_chrome();
                app.win.set_cursor_from_name(None);
            }
        ));
        self.win.add_controller(motion);

        // A frameless window has no decorations to grab, so the edges are
        // the app's job (FR-6.4). Capture phase, and claiming only inside
        // the margin, puts the edge ahead of the pan and window-move
        // drags that start on the image while leaving them untouched
        // everywhere else.
        let resize = gtk::GestureDrag::new();
        resize.set_propagation_phase(gtk::PropagationPhase::Capture);
        resize.connect_drag_begin(clone!(
            #[strong(rename_to = app)]
            self,
            move |gesture, x, y| {
                let Some(edge) = app.resize_edge(x, y) else {
                    return;
                };
                let Some(surface) = app.win.surface() else {
                    return;
                };
                let Ok(toplevel) = surface.downcast::<gdk::Toplevel>() else {
                    return;
                };
                let Some(device) = gesture.device() else {
                    return;
                };
                gesture.set_state(gtk::EventSequenceState::Claimed);
                // The compositor owns the drag from here (Wayland has no
                // client-side window geometry).
                toplevel.begin_resize(
                    edge,
                    Some(&device),
                    i32::try_from(gdk::BUTTON_PRIMARY).unwrap_or(1),
                    x,
                    y,
                    gesture.current_event_time(),
                );
            }
        ));
        self.win.add_controller(resize);

        // A pointer parked on the controls generates no motion, so the
        // fade timer would run out under it and take the click target
        // away. Hovering holds them until the pointer moves off.
        for w in &self.chrome {
            let hover = gtk::EventControllerMotion::new();
            hover.connect_enter(clone!(
                #[strong(rename_to = app)]
                self,
                move |_, _, _| {
                    app.pointer_on_chrome.set(true);
                    app.update_cursor();
                }
            ));
            hover.connect_leave(clone!(
                #[strong(rename_to = app)]
                self,
                move |_| {
                    app.pointer_on_chrome.set(false);
                    app.show_chrome();
                    app.update_cursor();
                }
            ));
            w.add_controller(hover);
        }
        // The status actions do not fade with media chrome, but resting the
        // pointer on one must still keep the cursor visible until it leaves.
        let status_hover = gtk::EventControllerMotion::new();
        status_hover.connect_enter(clone!(
            #[strong(rename_to = app)]
            self,
            move |_, _, _| app.pointer_on_status.set(true)
        ));
        status_hover.connect_leave(clone!(
            #[strong(rename_to = app)]
            self,
            move |_| {
                app.pointer_on_status.set(false);
                app.show_chrome();
            }
        ));
        self.status_area.add_controller(status_hover);

        // Double-click: fullscreen. Middle-click: fit/100% toggle (FR-4.3).
        let click = gtk::GestureClick::new();
        click.set_button(gdk::BUTTON_PRIMARY);
        click.connect_pressed(clone!(
            #[strong(rename_to = app)]
            self,
            move |_, n_press, _, _| {
                if n_press == 2 {
                    WidgetExt::activate_action(&app.win, &Action::Fullscreen.detailed_name(), None)
                        .ok();
                }
            }
        ));
        self.win.add_controller(click);
        let middle = gtk::GestureClick::new();
        middle.set_button(gdk::BUTTON_MIDDLE);
        middle.connect_pressed(clone!(
            #[strong(rename_to = app)]
            self,
            move |_, _, _, _| app.view.toggle_fit_actual()
        ));
        self.win.add_controller(middle);

        // Dragging the (non-pannable) image moves the window (FR-6.4).
        // The viewer's pan gesture claims the drag first when zoomed in.
        let drag = gtk::GestureDrag::new();
        let began = Rc::new(Cell::new(false));
        drag.connect_drag_begin(clone!(
            #[strong]
            began,
            move |_, _, _| began.set(false)
        ));
        drag.connect_drag_update(clone!(
            #[strong(rename_to = app)]
            self,
            #[strong]
            began,
            move |gesture, dx, dy| {
                if app.view.is_marking_up() {
                    return;
                }
                if began.get() || (dx * dx + dy * dy) < 36.0 {
                    return;
                }
                let Some(surface) = app.win.surface() else {
                    return;
                };
                let Ok(toplevel) = surface.downcast::<gdk::Toplevel>() else {
                    return;
                };
                let Some(device) = gesture.device() else {
                    return;
                };
                let (sx, sy) = gesture.start_point().unwrap_or((0.0, 0.0));
                began.set(true);
                gesture.set_state(gtk::EventSequenceState::Claimed);
                toplevel.begin_move(
                    &device,
                    i32::try_from(gdk::BUTTON_PRIMARY).unwrap_or(1),
                    sx,
                    sy,
                    gesture.current_event_time(),
                );
            }
        ));
        self.win.add_controller(drag);

        // Nautilus sends `GdkFileList`, even for one file. Accept that
        // native payload for real Files drags; keep GioFile as a fallback
        // for sources that advertise only a single file (FR-1.5/10.7).
        let file_list_drop =
            gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        file_list_drop.connect_drop(clone!(
            #[strong(rename_to = app)]
            self,
            move |_, value, _, _| {
                if let Ok(files) = value.get::<gdk::FileList>() {
                    return app.handle_dropped_files(files.files());
                }
                false
            }
        ));
        self.win.add_controller(file_list_drop);

        let single_file_drop =
            gtk::DropTarget::new(gio::File::static_type(), gdk::DragAction::COPY);
        single_file_drop.connect_drop(clone!(
            #[strong(rename_to = app)]
            self,
            move |_, value, _, _| value
                .get::<gio::File>()
                .is_ok_and(|file| app.handle_dropped_files(vec![file]))
        ));
        self.win.add_controller(single_file_drop);
    }
}
fn osd_button(icon: &str, action: &str, tooltip: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.set_action_name(Some(action));
    b.set_tooltip_text(Some(tooltip));
    b.add_css_class("flat");
    b.add_css_class("osd-btn");
    b.set_margin_start(12);
    b.set_margin_end(12);
    b.set_margin_top(12);
    b.set_margin_bottom(12);
    b
}

/// One cheat-sheet row, padded to `width`. The keys are padded *before*
/// escaping, so markup entities never count toward the column — `&lt;` is
/// one character on screen and four in the string. The width is measured
/// from the rows themselves rather than fixed, so a long accelerator
/// list widens the column instead of spilling out of it.
fn help_line(keys: &str, description: &str, width: usize) -> String {
    format!(
        "<tt>{}</tt> {description}",
        glib::markup_escape_text(&format!("{keys:<width$}"))
    )
}

fn bar_button(icon: &str, action: &str, tooltip: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.set_action_name(Some(action));
    b.set_tooltip_text(Some(tooltip));
    b.add_css_class("flat");
    b
}

pub(super) fn playback_speed_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    for rate in PLAYBACK_RATES {
        let item = gio::MenuItem::new(Some(&format_playback_rate(*rate)), None);
        item.set_action_and_target_value(
            Some(&Action::SpeedSet.detailed_name()),
            Some(&rate.to_variant()),
        );
        menu.append_item(&item);
    }
    menu
}

fn apply_css(background: &str) {
    let css = format!(
        r#"
        window {{ background-color: {background}; }}
        .chrome {{ transition: opacity 200ms ease; opacity: 1; }}
        .chrome.invisible {{ opacity: 0; }}
        .osd-surface, .osd-btn {{ background-color: rgba(0, 0, 0, 0.62);
            color: #eeeeee; border-radius: 9px; }}
        .info-bar {{ padding: 7px 11px; margin: 12px; }}
        .info-bar .position {{ opacity: 0.68; }}
        .window-controls {{ padding: 2px; margin: 12px; }}
        .window-controls button, .osd-bar button, .osd-bar menubutton > button {{
            min-width: 36px; min-height: 36px; padding: 0; border-radius: 7px; }}
        .osd-bar menubutton > button {{ background-color: transparent;
            background-image: none; border: none; box-shadow: none; }}
        .window-controls button:hover, .osd-bar button:hover,
        .osd-bar menubutton > button:hover {{
            background-color: rgba(255, 255, 255, 0.14); }}
        .indicator {{ transition: opacity 200ms ease; opacity: 1;
            background-color: rgba(0, 0, 0, 0.55); color: #eeeeee;
            padding: 4px 12px; margin: 12px; border-radius: 6px; }}
        .indicator.invisible {{ opacity: 0; }}
        .osd-btn {{ background-image: none; border: none; box-shadow: none; }}
        .osd-bar {{ padding: 4px 6px; margin: 12px; }}
        .markup-controls button:checked {{ background-color: rgba(255, 255, 255, 0.22); }}
        .osd-bar separator {{ min-width: 1px; margin: 5px 2px;
            background-color: rgba(255, 255, 255, 0.2); }}
        .toast {{ background-color: rgba(25, 25, 25, 0.92); color: #eeeeee;
            padding: 8px 14px; border-radius: 8px; }}
        .status {{ color: #999999; font-size: 1.1em; }}
        .status-actions button {{ background-color: rgba(25, 25, 25, 0.92);
            color: #eeeeee; border-radius: 7px; }}
        .status-actions button:hover {{ background-color: rgba(55, 55, 55, 0.96); }}
        .help {{ background-color: rgba(0, 0, 0, 0.85); color: #dddddd;
            padding: 18px 24px; border-radius: 10px; }}
        "#
    );
    let provider = gtk::CssProvider::new();
    provider.load_from_string(&css);
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

fn open_menu_model() -> gio::Menu {
    let menu = gio::Menu::new();
    menu.append(Some("Open File…"), Some(&Action::OpenFile.detailed_name()));
    menu.append(
        Some("Open Folder…"),
        Some(&Action::OpenFolder.detailed_name()),
    );
    menu
}

#[cfg(test)]
mod tests {
    use super::*;
    use gtk4::prelude::MenuModelExt;

    #[test]
    fn both_open_actions_share_the_more_and_context_menu_model() {
        let menu = open_menu_model();
        assert_eq!(menu.n_items(), 2);
        let action = |index| {
            menu.item_attribute_value(index, "action", Some(gtk4::glib::VariantTy::STRING))
                .and_then(|value| value.str().map(str::to_owned))
        };
        assert_eq!(action(0), Some("win.open-file".to_string()));
        assert_eq!(action(1), Some("win.open-folder".to_string()));
    }

    #[test]
    fn cheat_sheet_rows_align_regardless_of_markup_escaping() {
        use super::help_line;

        // What the user actually sees in the key column, entities
        // resolved back to the one character each stands for.
        let visible = |s: &str| {
            s.split("<tt>")
                .nth(1)
                .unwrap()
                .split("</tt>")
                .next()
                .unwrap()
                .replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&amp;", "&")
                .chars()
                .count()
        };
        let plain = help_line("Right", "Next image", 22);
        let escaped = help_line("<Control>z", "Undo", 22);
        assert!(plain.ends_with("</tt> Next image"), "{plain}");
        assert!(
            escaped.contains("&lt;Control&gt;z"),
            "markup must be escaped: {escaped}"
        );
        assert_eq!(visible(&plain), 22);
        assert_eq!(
            visible(&escaped),
            22,
            "an escaped key must occupy the same column: {escaped}"
        );
    }
}
