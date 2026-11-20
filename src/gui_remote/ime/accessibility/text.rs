//! Read-only text run boundaries and publication-local selection endpoints.

// Rust guideline compliant 2026-10-07
use super::{Entry, MAX_TEXT, read_items};
use crate::gui::accesskit::{
    Affine, Node, NodeId, Role, TextDirection, TextPosition, TextSelection,
};
use serde::{Deserialize, Serialize};

pub(super) fn provider(role: Role) -> bool {
    matches!(
        role,
        Role::Document | Role::Terminal | Role::Label | Role::Paragraph
    )
}

#[derive(Clone, Copy, Default)]
pub(super) enum Scope {
    #[default]
    Unspecified,
    Readable,
    Field,
}
impl Scope {
    pub(super) fn enter(self, role: Role, read_only: bool) -> Self {
        if matches!(self, Self::Field)
            || matches!(
                role,
                Role::TextInput
                    | Role::MultilineTextInput
                    | Role::SearchInput
                    | Role::DateInput
                    | Role::DateTimeInput
                    | Role::WeekInput
                    | Role::MonthInput
                    | Role::TimeInput
                    | Role::EmailInput
                    | Role::NumberInput
                    | Role::PasswordInput
                    | Role::PhoneNumberInput
                    | Role::UrlInput
                    | Role::EditableComboBox
            )
        {
            Self::Field
        } else if read_only && provider(role) {
            Self::Readable
        } else {
            self
        }
    }
    pub(super) fn readable(self) -> bool {
        matches!(self, Self::Readable)
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Run {
    value: String,
    #[serde(deserialize_with = "read_items::<_, _, MAX_TEXT>")]
    lengths: Vec<u8>,
    #[serde(deserialize_with = "read_items::<_, _, MAX_TEXT>")]
    words: Vec<u8>,
    direction: Option<u8>,
    metrics: Option<Metrics>,
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
struct Metrics(#[serde(deserialize_with = "read_items::<_, _, MAX_TEXT>")] Vec<[f32; 2]>);

impl Run {
    pub(super) fn capture(
        node: &Node,
        budget: usize,
        transform: Affine,
        viewport: [f32; 2],
    ) -> Option<Self> {
        let value = node.value()?;
        if value.len() > budget
            || node.character_lengths().len() > value.len()
            || node.word_starts().len() > node.character_lengths().len()
        {
            return None;
        }
        let run = Self {
            value: value.into(),
            lengths: node.character_lengths().into(),
            words: node.word_starts().into(),
            direction: node.text_direction().map(|v| v as u8),
            metrics: Self::capture_metrics(node, transform, viewport).map(Metrics),
        };
        run.valid().then_some(run)
    }
    fn capture_metrics(
        node: &Node,
        transform: Affine,
        viewport: [f32; 2],
    ) -> Option<Vec<[f32; 2]>> {
        let positions = node.character_positions()?;
        let widths = node.character_widths()?;
        if positions.len() != node.character_lengths().len() || widths.len() != positions.len() {
            return None;
        }
        let [sx, b, c, sy, _, _] = transform.as_coeffs();
        if b != 0. || c != 0. || sx <= 0. || sy <= 0. {
            return None;
        }
        let rect = transform.transform_rect_bbox(node.bounds()?);
        let direction = node.text_direction()?;
        let (lo, hi, limit, scale) = match direction {
            TextDirection::LeftToRight | TextDirection::RightToLeft => {
                (rect.x0, rect.x1, f64::from(viewport[0]), sx)
            }
            TextDirection::TopToBottom | TextDirection::BottomToTop => {
                (rect.y0, rect.y1, f64::from(viewport[1]), sy)
            }
        };
        let start = lo.clamp(0., limit);
        let end = hi.clamp(0., limit);
        let span = end - start;
        if !span.is_finite() || span <= 0. {
            return None;
        }
        let trimmed = match direction {
            TextDirection::LeftToRight | TextDirection::TopToBottom => start - lo,
            TextDirection::RightToLeft | TextDirection::BottomToTop => hi - end,
        };
        let normalize = |p: f64| ((scale * p - trimmed) / span).clamp(0., 1.) as f32;
        positions
            .iter()
            .zip(widths)
            .map(|(&p, &w)| {
                if !p.is_finite() || !w.is_finite() || w < 0. {
                    return None;
                }
                Some([
                    normalize(f64::from(p)),
                    normalize(f64::from(p) + f64::from(w)),
                ])
            })
            .collect()
    }
    pub(super) fn bytes(&self) -> usize {
        self.value.len()
    }
    pub(super) fn valid(&self) -> bool {
        if self.metrics.as_ref().is_some_and(|Metrics(m)| {
            self.direction.is_none()
                || m.len() != self.lengths.len()
                || m.iter()
                    .any(|[a, b]| !(0.0..=1.0).contains(a) || !(0.0..=1.0).contains(b) || a > b)
        }) || self.value.len() > MAX_TEXT
            || self.lengths.len() > self.value.len()
            || self.words.len() > self.lengths.len()
            || self
                .direction
                .is_some_and(|v| TextDirection::n(v).is_none())
            || !self.words.windows(2).all(|w| w[0] < w[1])
            || self
                .words
                .last()
                .is_some_and(|&w| usize::from(w) >= self.lengths.len())
        {
            return false;
        }
        let mut end = 0;
        for &len in &self.lengths {
            end += usize::from(len);
            if len == 0 || !self.value.is_char_boundary(end) {
                return false;
            }
        }
        end == self.value.len()
    }
    pub(super) fn apply(&self, node: &mut Node) {
        node.set_value(self.value.clone());
        node.set_character_lengths(self.lengths.clone());
        node.set_word_starts(self.words.clone());
        if let Some(direction) = self.direction.and_then(TextDirection::n) {
            node.set_text_direction(direction);
            if let (Some(Metrics(metrics)), Some(rect)) = (&self.metrics, node.bounds()) {
                let span = match direction {
                    TextDirection::LeftToRight | TextDirection::RightToLeft => rect.width(),
                    TextDirection::TopToBottom | TextDirection::BottomToTop => rect.height(),
                } as f32;
                node.set_character_positions(
                    metrics.iter().map(|[a, _]| a * span).collect::<Vec<_>>(),
                );
                node.set_character_widths(
                    metrics
                        .iter()
                        .map(|[a, b]| (b - a) * span)
                        .collect::<Vec<_>>(),
                );
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Selection {
    anchor: (u64, usize),
    focus: (u64, usize),
}
impl From<&TextSelection> for Selection {
    fn from(s: &TextSelection) -> Self {
        Self {
            anchor: (s.anchor.node.0, s.anchor.character_index),
            focus: (s.focus.node.0, s.focus.character_index),
        }
    }
}
impl Selection {
    pub(super) fn valid(self, provider: usize, entries: &[Entry]) -> bool {
        [self.anchor, self.focus].into_iter().all(|(id, offset)| {
            let Some(mut i) = entries.iter().position(|n| n.id == id) else {
                return false;
            };
            if entries[i]
                .run
                .as_ref()
                .is_none_or(|r| offset > r.lengths.len())
            {
                return false;
            }
            while i > provider {
                let Some(parent) = entries[i].parent.map(usize::from).filter(|&p| p < i) else {
                    return false;
                };
                i = parent;
            }
            i == provider
        })
    }
    pub(super) fn apply(self, node: &mut Node, resolve: &impl Fn(u64) -> NodeId) {
        let position = |(id, character_index)| TextPosition {
            node: resolve(id),
            character_index,
        };
        node.set_text_selection(TextSelection {
            anchor: position(self.anchor),
            focus: position(self.focus),
        });
    }
}

#[cfg(all(test, feature = "gui-testing"))]
mod tests {
    use super::*;
    use crate::gui::Styled as _;
    use crate::gui::{
        self, AppContext as _, Context, InteractiveElement as _, IntoElement, ParentElement as _,
        Render, StatefulInteractiveElement as _, TestAppContext, Window,
        accesskit::{Tree, TreeId, TreeUpdate},
    };
    use crate::gui_remote::ime::AccessibilityTree;
    use std::{cell::RefCell, rc::Rc};

    fn document(label: &str) -> TreeUpdate {
        let mut doc = Node::new(Role::Document);
        doc.set_read_only();
        doc.set_label(label);
        doc.set_children(vec![NodeId(2), NodeId(3), NodeId(6)]);
        doc.set_text_selection(TextSelection {
            anchor: TextPosition {
                node: NodeId(4),
                character_index: 2,
            },
            focus: TextPosition {
                node: NodeId(2),
                character_index: 1,
            },
        });
        let mut first = Node::new(Role::TextRun);
        first.set_value("Ae\u{301}");
        first.set_character_lengths(vec![1, 3]);
        first.set_word_starts(vec![0]);
        first.set_text_direction(TextDirection::LeftToRight);
        first.set_bounds(gui::accesskit::Rect::new(-10., 0., 30., 10.));
        first.set_character_positions(vec![0., 20.]);
        first.set_character_widths(vec![20., 20.]);
        let mut line = Node::new(Role::Paragraph);
        line.set_children(vec![NodeId(4)]);
        let mut second = Node::new(Role::TextRun);
        second.set_value("🙂א");
        second.set_character_lengths(vec![4, 2]);
        second.set_text_direction(TextDirection::RightToLeft);
        second.set_bounds(gui::accesskit::Rect::new(110., 0., 130., 10.));
        second.set_character_positions(vec![0., 10.]);
        second.set_character_widths(vec![10., 10.]);
        let mut field = Node::new(Role::TextInput);
        field.set_value("private edit value");
        field.set_children(vec![NodeId(7)]);
        let mut nested = Node::new(Role::Label);
        nested.set_read_only();
        nested.set_value("private descendant");
        TreeUpdate {
            nodes: vec![
                (NodeId(1), doc),
                (NodeId(2), first),
                (NodeId(3), line),
                (NodeId(4), second),
                (NodeId(6), field),
                (NodeId(7), nested),
            ],
            tree: Some(Tree::new(NodeId(1))),
            tree_id: TreeId::ROOT,
            focus: NodeId(1),
        }
    }
    fn capture(update: &TreeUpdate) -> AccessibilityTree {
        AccessibilityTree::capture(update, [120., 80.], false).unwrap()
    }
    #[test]
    fn validates_text_units_and_selection_scope_on_both_ends() {
        let mut update = document("Page");
        let payload = serde_json::to_value(capture(&update)).unwrap();
        let metrics = &payload["entries"][1]["run"]["metrics"];
        assert!((metrics[0][1].as_f64().unwrap() - 1. / 3.).abs() < 1e-6);
        assert_eq!(metrics[0][1], metrics[1][0]);
        assert_eq!(
            payload["entries"][3]["run"]["metrics"],
            serde_json::json!([[0., 0.], [0., 1.]])
        );
        assert!(!payload.to_string().contains("private"));
        for (entry, key, value) in [
            (
                1,
                "run",
                serde_json::json!({"value":"é","lengths":[1,1],"words":[],"direction":null}),
            ),
            (
                1,
                "run",
                serde_json::json!({"value":"ab","lengths":[1,1],"words":[1,0],"direction":null}),
            ),
            (
                0,
                "selection",
                serde_json::json!({"anchor":[2,3],"focus":[4,1]}),
            ),
            (
                2,
                "selection",
                serde_json::json!({"anchor":[2,1],"focus":[4,1]}),
            ),
            (5, "value", serde_json::json!("private descendant")),
        ] {
            let mut malformed = payload.clone();
            malformed["entries"][entry][key] = value;
            assert!(serde_json::from_value::<AccessibilityTree>(malformed).is_err());
        }
        update.nodes[1].1.set_character_lengths(vec![1, 1, 1, 1]);
        assert!(AccessibilityTree::capture(&update, [120., 80.], false).is_none());
        let mut update = document("Page");
        update.nodes[1].1.set_hidden();
        let hidden = serde_json::to_value(capture(&update)).unwrap();
        assert!(hidden["entries"][0]["selection"].is_null());
        let mut update = document("Page");
        update.nodes[4].1.set_role(Role::PasswordInput);
        let password = serde_json::to_value(capture(&update)).unwrap();
        assert_eq!(password["entries"].as_array().unwrap().len(), 5);
        assert!(!password.to_string().contains("private"));
    }

    #[test]
    fn projected_selections_resolve_only_into_their_own_unicode_runs() {
        struct Panes([Rc<AccessibilityTree>; 2]);
        impl Render for Panes {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                gui::div().children(self.0.iter().enumerate().map(|(i, tree)| {
                    let tree = tree.clone();
                    gui::div()
                        .id(("pane", i))
                        .w(gui::px(240.))
                        .h(gui::px(160.))
                        .role(Role::Group)
                        .a11y_synthetic_children(move |b| tree.populate(b, 1))
                }))
            }
        }
        let mut test = TestAppContext::single();
        let pane = test.add_window(|_, _| {
            Panes(["First", "Peer"].map(|label| {
                Rc::new(
                    serde_json::from_value(
                        serde_json::to_value(capture(&document(label))).unwrap(),
                    )
                    .unwrap(),
                )
            }))
        });
        let latest = Rc::new(RefCell::new(None));
        test.update_window(pane.into(), |_, window, cx| {
            let latest = latest.clone();
            window.observe_hosted_accessibility(move |tree, _| {
                *latest.borrow_mut() = Some(tree.clone())
            });
            window.draw(cx).clear(cx);
        })
        .unwrap();
        let snapshot = latest.borrow();
        let nodes = &snapshot.as_ref().unwrap().nodes;
        let mut targets = std::collections::HashSet::new();
        for label in ["First", "Peer"] {
            let doc = &nodes
                .iter()
                .find(|(_, n)| n.label() == Some(label))
                .unwrap()
                .1;
            let find = |id| &nodes.iter().find(|(i, _)| *i == id).unwrap().1;
            let selection = doc.text_selection().unwrap();
            assert!(doc.is_read_only());
            assert_eq!(selection.focus.node, doc.children()[0]);
            assert_eq!(selection.anchor.node, find(doc.children()[1]).children()[0]);
            assert_eq!(
                (
                    selection.anchor.character_index,
                    selection.focus.character_index
                ),
                (2, 1)
            );
            for target in [selection.anchor.node, selection.focus.node] {
                assert!(targets.insert(target));
            }
            let a = find(selection.anchor.node);
            let f = find(selection.focus.node);
            assert_eq!(a.value(), Some("🙂א"));
            assert_eq!(a.text_direction(), Some(TextDirection::RightToLeft));
            assert_eq!(a.character_lengths(), &[4, 2]);
            assert_eq!(f.role(), Role::TextRun);
            assert_eq!(f.value(), Some("Ae\u{301}"));
            assert_eq!(f.character_lengths(), &[1, 3]);
            assert_eq!(f.word_starts(), &[0]);
            let width = f.bounds().unwrap().width() as f32;
            assert!(width > 0.);
            let positions = f.character_positions().unwrap();
            let widths = f.character_widths().unwrap();
            assert!((positions[1] - width / 3.).abs() < 1e-4);
            assert!((widths[0] - width / 3.).abs() < 1e-4);
            assert!((widths[1] - width * 2. / 3.).abs() < 1e-4);
            assert_eq!(
                a.character_widths().unwrap(),
                &[0., a.bounds().unwrap().width() as f32]
            );
        }
    }
}
