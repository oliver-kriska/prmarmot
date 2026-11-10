//! The Details panel: the selected PR's fields, beside or below the board.

use super::*;

impl RootView {
    /// Details' rows: core's facts as a muted label beside its value (the
    /// Note first, unlabelled), a hairline between them, then the attention
    /// and snooze lines, muted. Labels are the chip row above, so their fact
    /// is left out here. `columns` (the wide, short bottom pane) puts who is
    /// involved in one group and the PR's health in another, side by side.
    pub(super) fn detail_field_rows(
        &self,
        row: &BoardRow,
        mode: Mode,
        columns: bool,
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = cx.theme();
        let state = self.state.read(cx);
        let fields = prmarmot_core::detail::detail_fields(
            row,
            mode,
            Utc::now(),
            crate::table::waiting_offset_secs(row),
        );
        let mut rows: Vec<AnyElement> = Vec::new();
        let mut health: Vec<AnyElement> = Vec::new();
        // Keyed by the PR, so a text selection never carries over to the
        // same field of the next PR.
        let field_key = SharedString::from(format!("detail-field-{}", row.id));
        for (ix, field) in fields.into_iter().enumerate() {
            use prmarmot_core::detail::DetailKind;
            // Author, reviewers, reviews, issue and stack say who and where;
            // CI, the checks, threads, the wait and the size say how the PR
            // stands.
            let group = if columns && field.standing {
                &mut health
            } else {
                &mut rows
            };
            match (field.kind, field.label) {
                (DetailKind::Labels, _) => {}
                // The Note: the same tone dot as the table's Note cell, so
                // the line that explains the row keeps its severity here too.
                (DetailKind::Note, None) => {
                    let tone = prmarmot_core::cells::note_presentation(row).tone;
                    let (dot, _) = crate::table::note_tone_colors(tone, theme);
                    rows.push(
                        h_flex()
                            .id(("detail-fact", ix))
                            .role(gpui::Role::Label)
                            .aria_value(field.value.clone())
                            .items_start()
                            .gap_1p5()
                            .pb(px(6.))
                            .border_b_1()
                            .border_color(theme.border)
                            .text_size(type_size::BODY)
                            .children(dot.map(|color| {
                                // Centred on the first line of a note that
                                // may wrap: (line height − dot) / 2.
                                crate::table::status_dot(color).mt(px(
                                    (crate::design::TABLE_TEXT_PX * 1.4
                                        - crate::design::STATUS_DOT)
                                        / 2.,
                                ))
                            }))
                            .child(
                                div().flex_1().min_w_0().child(SelectableText::new(
                                    (field_key.clone(), ix),
                                    field.value,
                                )),
                            )
                            .into_any_element(),
                    );
                }
                (_, None) => rows.push(
                    div()
                        .id(("detail-fact", ix))
                        .role(gpui::Role::Label)
                        .aria_value(field.value.clone())
                        .pb(px(6.))
                        .border_b_1()
                        .border_color(theme.border)
                        .text_size(type_size::BODY)
                        .child(SelectableText::new((field_key.clone(), ix), field.value))
                        .into_any_element(),
                ),
                (_, Some(label)) => {
                    // The pages the value names (a failed check's run) as
                    // chips under the value, each opening in the browser.
                    let links = field.links;
                    group.push(
                        h_flex()
                            .id(("detail-fact", ix))
                            .role(gpui::Role::Label)
                            .aria_value(format!("{label}: {}", field.value))
                            .items_start()
                            .gap_2()
                            .pb(px(6.))
                            .border_b_1()
                            .border_color(theme.border)
                            .child(
                                div()
                                    .w(px(DETAILS_LABEL_WIDTH))
                                    .flex_shrink_0()
                                    .text_size(type_size::SMALL)
                                    .text_color(theme.muted_foreground)
                                    .child(label),
                            )
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap_1()
                                    .text_size(type_size::BODY)
                                    .child(SelectableText::new(
                                        (field_key.clone(), ix),
                                        field.value,
                                    ))
                                    .when(!links.is_empty(), |value| {
                                        value.child(h_flex().flex_wrap().gap_1().children(
                                            links.into_iter().enumerate().map(|(lx, link)| {
                                                let tip = format!("Open the {} run", link.text);
                                                let url = link.url;
                                                label_chip(theme, crate::skin::glacier(cx), cx)
                                                    .id(("detail-link", ix * 64 + lx))
                                                    .role(gpui::Role::Link)
                                                    .aria_label(tip.clone())
                                                    .cursor_pointer()
                                                    .text_color(theme.secondary_foreground)
                                                    .hover(|style| {
                                                        style
                                                            .border_color(theme.muted_foreground)
                                                            .text_color(theme.foreground)
                                                    })
                                                    .child(format!("{} ↗", link.text))
                                                    .tooltip(move |window, cx| {
                                                        Tooltip::new(tip.clone()).build(window, cx)
                                                    })
                                                    .on_click(move |_, _, cx| {
                                                        crate::open::open_web_url(cx, &url);
                                                    })
                                            }),
                                        ))
                                    }),
                            )
                            .into_any_element(),
                    )
                }
            }
        }
        if columns {
            // The Note (unlabelled) stays above both groups.
            let people: Vec<AnyElement> = rows.drain(1.min(rows.len())..).collect();
            rows.push(
                h_flex()
                    .items_start()
                    .gap_6()
                    .child(v_flex().flex_1().min_w_0().gap_2().children(people))
                    .child(v_flex().flex_1().min_w_0().gap_2().children(health))
                    .into_any_element(),
            );
        }
        let mut closing = vec![prmarmot_core::detail::attention_line(
            state.is_changed(&row.id),
            state.is_watched(&row.id),
        )];
        closing.extend(state.snooze_description(&row.id));
        rows.extend(closing.into_iter().enumerate().map(|(ix, line)| {
            div()
                .id(("detail-closing", ix))
                .role(gpui::Role::Label)
                .aria_value(line.clone())
                .text_size(type_size::SMALL)
                .text_color(theme.muted_foreground)
                .child(line)
                .into_any_element()
        }));
        rows
    }

    /// Details at the bottom (`right` false: a 210 px pane, the facts as
    /// sentences) or on the right (a full-height panel, the facts as label and
    /// value rows). Both read core's `detail` module, so they say the same.
    pub(super) fn render_details(&self, right: bool, cx: &Context<Self>) -> impl IntoElement {
        let table = self.table.read(cx);
        let selected = table.selected_row().and_then(|ix| table.delegate().row(ix));
        let mode = self.state.read(cx).mode;
        let theme = cx.theme();
        let (hover_border, hover_text) = (theme.muted_foreground, theme.foreground);
        let field_rows: Vec<AnyElement> = selected
            .map(|row| self.detail_field_rows(row, mode, !right, cx))
            .unwrap_or_default();
        // The PR's title and its labels as chips, on one line when they fit;
        // a chip click filters by the label, as in the table. It heads the
        // right panel, and takes the place of "PR details" in the bottom pane
        // so the short pane keeps room for the facts.
        let heading = selected.map(|row| {
            h_flex()
                .flex_wrap()
                .items_center()
                .gap_x_3()
                .gap_y_1()
                .child(
                    div()
                        .id(SharedString::from(format!("detail-heading-{}", row.id)))
                        .role(gpui::Role::Heading)
                        .aria_label(format!("#{}  {}", row.number, row.title))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(SelectableText::new(
                            SharedString::from(format!("detail-title-{}", row.id)),
                            format!("#{}  {}", row.number, row.title),
                        )),
                )
                .when(!row.labels.is_empty(), |heading| {
                    heading.child(
                        h_flex()
                            .flex_wrap()
                            .gap_1()
                            .text_size(type_size::BODY)
                            .children(row.labels.iter().enumerate().map(|(ix, label)| {
                                let target = FilterChip::new(Qualifier::Label, label);
                                let tip = format!("Filter by {}", target.term());
                                label_chip(theme, crate::skin::glacier(cx), cx)
                                    .id(("detail-label", ix))
                                    .role(gpui::Role::Button)
                                    .aria_label(tip.clone())
                                    .cursor_pointer()
                                    .text_color(theme.secondary_foreground)
                                    .hover(|style| {
                                        style.border_color(hover_border).text_color(hover_text)
                                    })
                                    .child(label.clone())
                                    .tooltip(move |window, cx| {
                                        Tooltip::new(tip.clone()).build(window, cx)
                                    })
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.filter_by(target.clone(), window, cx)
                                    }))
                            })),
                    )
                })
        });
        let (bar_heading, content_heading) = if right {
            (None, heading)
        } else {
            (heading, None)
        };
        v_flex()
            .map(|panel| {
                if right {
                    panel.w(px(DETAILS_PANEL_WIDTH)).h_full().border_l_1()
                } else {
                    // Glacier's wider padding and gaps (below) need the
                    // room back, or the fourth row is cut in half.
                    let height = if crate::skin::glacier(cx) { 228. } else { 210. };
                    panel.h(px(height)).border_t_1()
                }
            })
            .flex_shrink_0()
            .border_color(theme.border)
            .bg(theme.background)
            .px(px(16.))
            // Glacier: a touch more air inside the sheet.
            .map(|panel| {
                if crate::skin::glacier(cx) {
                    panel.py(px(12.)).gap(px(12.))
                } else {
                    panel.py(px(10.)).gap_2()
                }
            })
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().min_w_0().map(|title| match bar_heading {
                        Some(heading) => title.child(heading),
                        None if !right => {
                            title.font_weight(FontWeight::SEMIBOLD).child("PR details")
                        }
                        None => title,
                    }))
                    .when_some(selected, |bar, row| {
                        // The menu reads the row when it opens, so a frame
                        // copies nothing for it.
                        let pr_id = row.id.clone();
                        let table = self.table.downgrade();
                        let view = cx.entity().downgrade();
                        bar.child(
                            crate::skin::quiet(Button::new("copy-detail"), cx)
                                .small()
                                .label("Copy")
                                .dropdown_caret(true)
                                .dropdown_menu(move |mut menu, _, cx| {
                                    let items =
                                        table
                                            .upgrade()
                                            .and_then(|table| {
                                                table.read(cx).delegate().row_by_id(&pr_id).map(
                                                    |row| crate::table::row_copy_items(row, mode),
                                                )
                                            })
                                            .unwrap_or_default();
                                    for (label, text) in items {
                                        let view = view.clone();
                                        menu = menu.item(PopupMenuItem::new(label).on_click(
                                            move |_, _, cx| {
                                                cx.write_to_clipboard(ClipboardItem::new_string(
                                                    text.clone(),
                                                ));
                                                let _ = view.update(cx, |this, cx| {
                                                    this.show_feedback("Copied to clipboard", cx)
                                                });
                                            },
                                        ));
                                    }
                                    menu
                                }),
                        )
                    })
                    .when_some(selected, |bar, row| {
                        let url = row.url.clone();
                        bar.child(
                            crate::skin::quiet(Button::new("open-detail"), cx)
                                .small()
                                .label("Open on GitHub")
                                .on_click(move |_, _, cx| {
                                    crate::open::open_web_url(cx, &url);
                                }),
                        )
                    })
                    .child(
                        crate::skin::quiet(Button::new("close-details"), cx)
                            .small()
                            .label("Close · Esc")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.set_details_open(false, cx);
                                this.table.focus_handle(cx).focus(window, cx);
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        v_flex()
                            // Keyed by the PR: each one opens scrolled to its
                            // top rather than where the previous one was left.
                            .id(SharedString::from(format!(
                                "pr-details-{}",
                                selected.map_or("", |row| row.id.as_str())
                            )))
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.details_scroll)
                            .gap_2()
                            .map(|content| match selected {
                                Some(_) => content.children(content_heading).children(field_rows),
                                None => content.child("Select a PR to inspect its details."),
                            }),
                    )
                    // When the fields go on below the pane, a fade of the
                    // pane's own colour over the last line says so; the
                    // scrollbar is still, so a cut line read as broken
                    // (critique 2026-10-21). The range is the last frame's,
                    // which is fine: the fields change with the selection.
                    .when(self.details_scroll.max_offset().y > px(0.), |pane| {
                        pane.child(
                            div()
                                .absolute()
                                .left_0()
                                .right_0()
                                .bottom_0()
                                .h(px(18.))
                                .bg(gpui::linear_gradient(
                                    180.,
                                    gpui::linear_color_stop(theme.background.opacity(0.), 0.),
                                    gpui::linear_color_stop(theme.background, 1.),
                                )),
                        )
                    }),
            )
    }
}
