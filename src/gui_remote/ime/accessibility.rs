//! Bounded projection of an independent raster window's accessibility tree.

// Rust guideline compliant 2026-10-07
mod text;
use crate::gui::{
    A11ySubtreeBuilder,
    accesskit::{Affine, Node, NodeId, Rect, Role, TreeUpdate},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fmt,
};

type ActionHandler = std::rc::Rc<dyn Fn(u64, TreeAction, &crate::gui::Window)>;

const MAX_NODES: usize = 128;
const MAX_DEPTH: usize = 24;
const MAX_TEXT: usize = 8192;
const MAX_FIELD: usize = 1024;
pub(super) const MAX_ENCODED_BYTES: usize = MAX_TEXT * 54 + MAX_NODES * 640;

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    id: u64,
    parent: Option<u8>,
    role: u8,
    bounds: Option<[f64; 4]>,
    label: String,
    description: String,
    disabled: bool,
    actions: u8,
    selected: Option<bool>,
    expanded: Option<bool>,
    toggled: Option<u8>,
    range: Option<RangeState>,
    modal: bool,
    read_only: bool,
    run: Option<text::Run>,
    value: Option<String>,
    selection: Option<text::Selection>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RangeState {
    value: Option<f64>,
    min: Option<f64>,
    max: Option<f64>,
    step: Option<f64>,
}
impl RangeState {
    fn supports(role: Role) -> bool {
        matches!(
            role,
            Role::SpinButton
                | Role::Slider
                | Role::Meter
                | Role::ProgressIndicator
                | Role::ScrollBar
        )
    }
    fn valid(&self) -> bool {
        [self.value, self.min, self.max, self.step]
            .into_iter()
            .flatten()
            .all(f64::is_finite)
            && self.step.is_none_or(|step| step > 0.)
            && self.min.zip(self.max).is_none_or(|(min, max)| min <= max)
            && self
                .min
                .zip(self.value)
                .is_none_or(|(min, value)| min <= value)
            && self
                .value
                .zip(self.max)
                .is_none_or(|(value, max)| value <= max)
    }
    fn apply(&self, node: &mut Node) {
        if let Some(v) = self.value {
            node.set_numeric_value(v);
        }
        if let Some(v) = self.min {
            node.set_min_numeric_value(v);
        }
        if let Some(v) = self.max {
            node.set_max_numeric_value(v);
        }
        if let Some(v) = self.step {
            node.set_numeric_value_step(v);
        }
    }
}

/// Data-free assistive actions supported by the raster presenter.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum TreeAction {
    /// Activate the specified control.
    Click,
    /// Move keyboard focus to the specified control.
    Focus,
    /// Increase the control's value by one step.
    Increment,
    /// Decrease the control's value by one step.
    Decrement,
    /// Open the control's collapsed content.
    Expand,
    /// Close the control's expanded content.
    Collapse,
}
impl TreeAction {
    const ALL: [Self; 6] = [
        Self::Click,
        Self::Focus,
        Self::Increment,
        Self::Decrement,
        Self::Expand,
        Self::Collapse,
    ];
    const fn mask(self) -> u8 {
        1 << self as u8
    }
    pub(super) fn native(self) -> crate::gui::AccessibleAction {
        match self {
            Self::Click => crate::gui::AccessibleAction::Click,
            Self::Focus => crate::gui::AccessibleAction::Focus,
            Self::Increment => crate::gui::AccessibleAction::Increment,
            Self::Decrement => crate::gui::AccessibleAction::Decrement,
            Self::Expand => crate::gui::AccessibleAction::Expand,
            Self::Collapse => crate::gui::AccessibleAction::Collapse,
        }
    }
}
/// A child window's accessible hierarchy, clipped to its presented raster pane.
#[derive(PartialEq, Serialize, Deserialize)]
#[serde(try_from = "WireTree")]
pub struct AccessibilityTree {
    entries: Vec<Entry>,
    focused: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireTree {
    #[serde(deserialize_with = "read_items::<_, _, MAX_NODES>")]
    entries: Vec<Entry>,
    focused: u64,
}
fn read_items<'de, D, T, const LIMIT: usize>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct Items<T, const LIMIT: usize>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>, const LIMIT: usize> serde::de::Visitor<'de> for Items<T, LIMIT> {
        type Value = Vec<T>;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "at most {LIMIT} items")
        }
        fn visit_seq<S: serde::de::SeqAccess<'de>>(self, mut seq: S) -> Result<Vec<T>, S::Error> {
            let mut items = Vec::new();
            while items.len() < LIMIT {
                let Some(item) = seq.next_element()? else {
                    return Ok(items);
                };
                items.push(item);
            }
            if seq.next_element::<serde::de::IgnoredAny>()?.is_some() {
                return Err(serde::de::Error::custom(
                    "accessibility list limit exceeded",
                ));
            }
            Ok(items)
        }
    }
    deserializer.deserialize_seq(Items::<T, LIMIT>(std::marker::PhantomData))
}
impl TryFrom<WireTree> for AccessibilityTree {
    type Error = &'static str;
    fn try_from(wire: WireTree) -> Result<Self, Self::Error> {
        let tree = Self {
            entries: wire.entries,
            focused: wire.focused,
        };
        tree.valid()
            .then_some(tree)
            .ok_or("invalid child accessibility tree")
    }
}
impl fmt::Debug for AccessibilityTree {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccessibilityTree")
            .field("nodes", &self.entries.len())
            .finish_non_exhaustive()
    }
}
impl AccessibilityTree {
    pub(super) fn capture(
        update: &TreeUpdate,
        viewport: [f32; 2],
        allow_click: bool,
    ) -> Option<Self> {
        if update.nodes.is_empty()
            || update.nodes.len() > MAX_NODES
            || !viewport.iter().all(|v| v.is_finite() && *v > 0.)
        {
            return None;
        }
        let nodes: HashMap<_, _> = update.nodes.iter().map(|(id, n)| (*id, n)).collect();
        if nodes.len() != update.nodes.len() {
            return None;
        }
        let root = update.tree.as_ref()?.root;
        let mut stack = vec![(root, None, Affine::IDENTITY, 0, text::Scope::Unspecified)];
        let mut seen = HashSet::new();
        let mut entries = Vec::new();
        let mut bytes = 0;
        while let Some((id, parent, transform, depth, scope)) = stack.pop() {
            if !seen.insert(id) || depth > MAX_DEPTH {
                return None;
            }
            let n = nodes.get(&id)?;
            if n.is_hidden() {
                continue;
            }
            let transform = transform * n.transform().copied().unwrap_or(Affine::IDENTITY);
            let label = n.label().unwrap_or_default();
            let description = n.description().unwrap_or_default();
            if label.len() > MAX_FIELD || description.len() > MAX_FIELD {
                return None;
            }
            bytes += label.len() + description.len();
            if bytes > MAX_TEXT
                || n.children().len() > nodes.len().saturating_sub(seen.len() + stack.len())
            {
                return None;
            }
            let scope = scope.enter(n.role(), n.is_read_only());
            let run = if scope.readable() && n.role() == Role::TextRun && n.value().is_some() {
                Some(text::Run::capture(
                    n,
                    MAX_TEXT - bytes,
                    transform,
                    viewport,
                )?)
            } else {
                None
            };
            bytes += run.as_ref().map_or(0, text::Run::bytes);
            let value = n
                .value()
                .filter(|_| scope.readable() && text::provider(n.role()));
            if value.is_some_and(|s| s.len() > MAX_TEXT - bytes) {
                return None;
            }
            bytes += value.map_or(0, str::len);
            let bounds = if let Some(rect) = n.bounds() {
                let r = transform.transform_rect_bbox(rect);
                let [w, h] = viewport.map(f64::from);
                let b = [r.x0 / w, r.y0 / h, r.x1 / w, r.y1 / h];
                if !b.iter().all(|n| n.is_finite()) {
                    return None;
                }
                Some(b.map(|n| n.clamp(0., 1.)))
            } else {
                None
            };
            let index = u8::try_from(entries.len()).ok()?;
            let range = RangeState::supports(n.role()).then(|| RangeState {
                value: n.numeric_value(),
                min: n.min_numeric_value(),
                max: n.max_numeric_value(),
                step: n.numeric_value_step(),
            });
            if range.as_ref().is_some_and(|r| !r.valid()) {
                return None;
            }
            let actions = TreeAction::ALL
                .into_iter()
                .filter(|a| {
                    (allow_click || !matches!(a, TreeAction::Click))
                        && n.supports_action(a.native())
                })
                .fold(0, |mask, a| mask | a.mask());
            entries.push(Entry {
                id: id.0,
                parent,
                role: n.role() as u8,
                label: label.into(),
                description: description.into(),
                bounds,
                disabled: n.is_disabled(),
                actions,
                range,
                selected: n.is_selected(),
                expanded: n.is_expanded(),
                toggled: n.toggled().map(|v| v as u8),
                modal: n.is_modal(),
                read_only: n.is_read_only(),
                run,
                value: value.map(str::to_owned),
                selection: (scope.readable() && text::provider(n.role()))
                    .then(|| n.text_selection().map(text::Selection::from))
                    .flatten(),
            });
            if n.role() == Role::PasswordInput {
                continue;
            }
            stack.extend(
                n.children()
                    .iter()
                    .rev()
                    .map(|child| (*child, Some(index), transform, depth + 1, scope)),
            );
        }
        if !entries.iter().any(|entry| entry.id == update.focus.0) {
            return None;
        }
        // An endpoint in a hidden or excluded branch cannot identify a presented run.
        for i in 0..entries.len() {
            if entries[i].selection.is_some_and(|s| !s.valid(i, &entries)) {
                entries[i].selection = None;
            }
        }
        Some(Self {
            entries,
            focused: update.focus.0,
        })
    }

    fn valid(&self) -> bool {
        if self.entries.is_empty() || self.entries.len() > MAX_NODES {
            return false;
        }
        let mut seen = HashSet::new();
        let mut depths = [0; MAX_NODES];
        let mut scopes = [text::Scope::Unspecified; MAX_NODES];
        let mut bytes = 0;
        for (i, n) in self.entries.iter().enumerate() {
            if !seen.insert(NodeId(n.id))
                || Role::n(n.role).is_none()
                || n.actions >= (1 << TreeAction::ALL.len())
                || n.toggled
                    .is_some_and(|v| crate::gui::Toggled::n(v).is_none())
                || n.range.as_ref().is_some_and(|r| {
                    !Role::n(n.role).is_some_and(RangeState::supports) || !r.valid()
                })
                || n.label.len() > MAX_FIELD
                || n.description.len() > MAX_FIELD
                || n.bounds.is_some_and(|b| {
                    !b.iter().all(|p| (0.0..=1.0).contains(p)) || b[0] > b[2] || b[1] > b[3]
                })
            {
                return false;
            }
            match n.parent.map(usize::from) {
                None if i == 0 => (),
                Some(p)
                    if p < i
                        && depths[p] < MAX_DEPTH
                        && self.entries[p].role != Role::PasswordInput as u8 =>
                {
                    depths[i] = depths[p] + 1
                }
                _ => return false,
            }
            let role = Role::n(n.role).expect("role validated above");
            let scope = n
                .parent
                .map_or(text::Scope::Unspecified, |p| scopes[usize::from(p)])
                .enter(role, n.read_only);
            scopes[i] = scope;
            if n.run
                .as_ref()
                .is_some_and(|r| role != Role::TextRun || !scope.readable() || !r.valid())
                || n.selection.is_some_and(|s| {
                    !scope.readable() || !text::provider(role) || !s.valid(i, &self.entries)
                })
            {
                return false;
            }
            if n.value
                .as_ref()
                .is_some_and(|v| !scope.readable() || !text::provider(role) || v.len() > MAX_TEXT)
            {
                return false;
            }
            bytes += n.value.as_ref().map_or(0, String::len);
            bytes += n.run.as_ref().map_or(0, text::Run::bytes);
            bytes += n.label.len() + n.description.len();
        }
        bytes <= MAX_TEXT && seen.contains(&NodeId(self.focused))
    }

    pub(super) fn supports(&self, node: u64, action: TreeAction) -> bool {
        let Some(mut i) = self.entries.iter().position(|entry| entry.id == node) else {
            return false;
        };
        let entry = &self.entries[i];
        if entry.actions & action.mask() == 0 {
            return false;
        }
        loop {
            if self.entries[i].disabled {
                return false;
            }
            let Some(parent) = self.entries[i].parent else {
                return true;
            };
            i = usize::from(parent);
        }
    }

    /// Whether a separate native input provider may accompany this child publication.
    /// Modal content and a focused password field take precedence over an editor excerpt.
    pub fn allows_editor_excerpt(&self) -> bool {
        if self.entries.iter().any(|entry| entry.modal) {
            return false;
        }
        let Some(mut entry) = self.entries.iter().find(|entry| entry.id == self.focused) else {
            return false;
        };
        loop {
            if Role::n(entry.role) == Some(Role::PasswordInput) {
                return false;
            }
            let Some(parent) = entry.parent else {
                return true;
            };
            entry = &self.entries[usize::from(parent)];
        }
    }

    /// Adds child nodes under the host pane, preserving child order and native focus.
    pub fn populate(&self, builder: &mut A11ySubtreeBuilder<'_>, epoch: u64) {
        if let Some(bounds) = builder.parent_node().bounds() {
            self.populate_root(builder, bounds, epoch, None, true);
        }
    }

    /// Adds the child tree and installs handlers for its enabled, supported actions.
    pub fn populate_with_actions(
        &self,
        builder: &mut A11ySubtreeBuilder<'_>,
        epoch: u64,
        dispatch: impl Fn(u64, TreeAction, &crate::gui::Window) + 'static,
    ) {
        self.populate_with_focus(builder, epoch, true, dispatch);
    }

    /// Installs actions while allowing a native editor to claim focus in the same pane.
    pub fn populate_with_focus(
        &self,
        builder: &mut A11ySubtreeBuilder<'_>,
        epoch: u64,
        claim_focus: bool,
        dispatch: impl Fn(u64, TreeAction, &crate::gui::Window) + 'static,
    ) {
        let Some(bounds) = builder.parent_node().bounds() else {
            return;
        };
        let dispatch: ActionHandler = std::rc::Rc::new(dispatch);
        self.populate_root(builder, bounds, epoch, Some(&dispatch), claim_focus);
    }
    fn populate_root(
        &self,
        builder: &mut A11ySubtreeBuilder<'_>,
        viewport: Rect,
        epoch: u64,
        dispatch: Option<&ActionHandler>,
        claim_focus: bool,
    ) {
        let ids: HashMap<_, _> = self
            .entries
            .iter()
            .map(|e| (e.id, builder.synthetic_node_id((epoch, e.id))))
            .collect();
        self.populate_node(0, builder, viewport, &ids, dispatch, claim_focus);
    }
    fn populate_node(
        &self,
        index: usize,
        builder: &mut A11ySubtreeBuilder<'_>,
        viewport: Rect,
        ids: &HashMap<u64, NodeId>,
        dispatch: Option<&ActionHandler>,
        claim_focus: bool,
    ) {
        let entry = &self.entries[index];
        let role = match Role::n(entry.role) {
            // TextRun requires glyph metadata. Expose its explicit label only.
            Some(Role::TextRun) if entry.run.is_none() => Role::Label,
            Some(Role::Window) | None => Role::Group,
            Some(role) => role,
        };
        let mut node = Node::new(role);
        if !entry.label.is_empty() {
            node.set_label(entry.label.clone());
        }
        if !entry.description.is_empty() {
            node.set_description(entry.description.clone());
        }
        if let Some(range) = &entry.range {
            range.apply(&mut node);
        }
        if let Some(v) = entry.selected {
            node.set_selected(v);
        }
        if let Some(v) = entry.expanded {
            node.set_expanded(v);
        }
        if let Some(v) = entry.toggled.and_then(crate::gui::Toggled::n) {
            node.set_toggled(v);
        }
        if entry.read_only {
            node.set_read_only();
        }
        if let Some(value) = &entry.value {
            node.set_value(value.clone());
        }
        if let Some(selection) = entry.selection {
            selection.apply(&mut node, &|id| ids[&id]);
        }
        if entry.modal {
            node.set_modal();
        }
        if entry.disabled {
            node.set_disabled();
        }
        if let Some([x0, y0, x1, y1]) = entry.bounds {
            node.set_bounds(Rect::new(
                viewport.x0 + x0 * viewport.width(),
                viewport.y0 + y0 * viewport.height(),
                viewport.x0 + x1 * viewport.width(),
                viewport.y0 + y1 * viewport.height(),
            ));
        }
        if let Some(run) = &entry.run {
            run.apply(&mut node);
        }
        let id = ids[&entry.id];
        builder.push_subtree(id, node, |subtree| {
            for action in TreeAction::ALL {
                if let Some(dispatch) = dispatch
                    && self.supports(entry.id, action)
                {
                    let dispatch = dispatch.clone();
                    let target = entry.id;
                    subtree.on_action(action.native(), move |_, window, _| {
                        dispatch(target, action, window)
                    });
                }
            }
            if claim_focus && entry.id == self.focused {
                subtree.mark_active_descendant();
            }
            for (child, _) in self
                .entries
                .iter()
                .enumerate()
                .skip(index + 1)
                .filter(|(_, n)| n.parent.map(usize::from) == Some(index))
            {
                self.populate_node(child, subtree, viewport, ids, dispatch, claim_focus);
            }
        });
    }
}

#[cfg(all(test, feature = "gui-testing"))]
mod tests {
    use super::*;
    use crate::gui::{
        self, AppContext as _, Context, InteractiveElement as _, IntoElement, Render,
        StatefulInteractiveElement as _, TestAppContext, Window,
    };
    use std::{cell::RefCell, rc::Rc};

    #[test]
    fn a_modal_or_focused_password_descendant_suppresses_the_native_excerpt() {
        let mut parent = Node::new(Role::PasswordInput);
        parent.set_children(vec![NodeId(2)]);
        let mut update = TreeUpdate {
            nodes: vec![(NodeId(1), parent), (NodeId(2), Node::new(Role::TextRun))],
            focus: NodeId(2),
            tree: Some(gui::accesskit::Tree::new(NodeId(1))),
            tree_id: gui::accesskit::TreeId::ROOT,
        };
        let allows = |update: &TreeUpdate| {
            AccessibilityTree::capture(update, [10., 10.], false)
                .is_some_and(|tree| tree.allows_editor_excerpt())
        };
        assert!(!allows(&update));
        update.focus = NodeId(1);
        assert!(!allows(&update));
        update.nodes[0].1.set_role(Role::Group);
        assert!(allows(&update));
        update.nodes[0].1.set_modal();
        assert!(!allows(&update));
    }

    #[test]
    fn disclosure_and_range_state_reach_the_presenter_adapter() {
        let mut row = Node::new(Role::TreeItem);
        row.set_label("Details");
        row.set_selected(false);
        row.set_expanded(false);
        row.set_toggled(gui::Toggled::Mixed);
        row.add_action(gui::AccessibleAction::Expand);
        row.set_children(vec![NodeId(2)]);
        let mut slider = Node::new(Role::Slider);
        slider.set_label("Volume");
        slider.set_min_numeric_value(-5.);
        slider.set_max_numeric_value(5.);
        slider.set_numeric_value(0.5);
        slider.set_numeric_value_step(0.25);
        slider.add_action(gui::AccessibleAction::Increment);
        let mut update = TreeUpdate {
            nodes: vec![(NodeId(1), row), (NodeId(2), slider)],
            focus: NodeId(1),
            tree: Some(gui::accesskit::Tree::new(NodeId(1))),
            tree_id: gui::accesskit::TreeId::ROOT,
        };
        let capture = |update: &TreeUpdate| {
            let tree = AccessibilityTree::capture(update, [100., 100.], false).unwrap();
            serde_json::from_slice::<AccessibilityTree>(&serde_json::to_vec(&tree).unwrap())
                .unwrap()
        };
        struct Presenter {
            tree: Rc<AccessibilityTree>,
            calls: Rc<RefCell<Vec<(u64, gui::AccessibleAction)>>>,
        }
        impl Render for Presenter {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                let tree = self.tree.clone();
                let calls = self.calls.clone();
                gui::div()
                    .id("pane")
                    .role(Role::Group)
                    .a11y_synthetic_children(move |builder| {
                        let calls = calls.clone();
                        tree.populate_with_actions(builder, 1, move |id, action, _| {
                            calls.borrow_mut().push((id, action.native()))
                        });
                    })
            }
        }
        let mut test = TestAppContext::single();
        let calls = Rc::new(RefCell::new(Vec::new()));
        let pane = test.add_window(|_, _| Presenter {
            tree: Rc::new(capture(&update)),
            calls: calls.clone(),
        });
        let latest = Rc::new(RefCell::new(None));
        assert!(test.simulate_accessibility_activation(pane.into(), true));
        test.update_window(pane.into(), |_, window, cx| {
            let latest = latest.clone();
            window.observe_hosted_accessibility(move |tree, _| {
                *latest.borrow_mut() = Some(tree.clone())
            });
            window.draw(cx).clear(cx);
        })
        .unwrap();
        for expanded in [false, true] {
            if expanded {
                let (_, row) = &mut update.nodes[0];
                row.set_expanded(true);
                row.set_selected(true);
                row.remove_action(gui::AccessibleAction::Expand);
                row.add_action(gui::AccessibleAction::Collapse);
                pane.update(&mut test, |pane, _, cx| {
                    pane.tree = Rc::new(capture(&update));
                    cx.notify();
                })
                .unwrap();
                test.update_window(pane.into(), |_, window, cx| window.draw(cx).clear(cx))
                    .unwrap();
            }
            let action = if expanded {
                gui::AccessibleAction::Collapse
            } else {
                gui::AccessibleAction::Expand
            };
            let (target, volume) = {
                let live = latest.borrow();
                let nodes = &live.as_ref().unwrap().nodes;
                let (id, row) = nodes
                    .iter()
                    .find(|(_, n)| n.label() == Some("Details"))
                    .unwrap();
                assert_eq!(row.is_selected(), Some(expanded));
                assert_eq!(row.is_expanded(), Some(expanded));
                assert_eq!(row.toggled(), Some(gui::Toggled::Mixed));
                assert!(row.supports_action(action));
                let (volume, slider) = nodes
                    .iter()
                    .find(|(id, _)| Some(id) == row.children().first())
                    .unwrap();
                assert_eq!(slider.numeric_value(), Some(0.5));
                assert_eq!(slider.min_numeric_value(), Some(-5.));
                assert_eq!(slider.max_numeric_value(), Some(5.));
                assert_eq!(slider.numeric_value_step(), Some(0.25));
                (*id, *volume)
            };
            assert!(test.simulate_accessibility_action(pane.into(), target, action));
            assert!(test.simulate_accessibility_action(
                pane.into(),
                volume,
                gui::AccessibleAction::Increment
            ));
            test.run_until_parked();
            assert_eq!(
                calls.borrow().as_slice(),
                &[(1, action), (2, gui::AccessibleAction::Increment)]
            );
            calls.borrow_mut().clear();
        }
        update.nodes[1].1.set_numeric_value(f64::NAN);
        assert!(AccessibilityTree::capture(&update, [100., 100.], true).is_none());
        update.nodes[1].1.set_numeric_value(6.);
        assert!(AccessibilityTree::capture(&update, [100., 100.], true).is_none());
        update.nodes[1].1.set_role(Role::PasswordInput);
        let payload = serde_json::to_value(capture(&update)).unwrap();
        assert!(payload["entries"][1]["range"].is_null());
        let mut forged = payload;
        forged["entries"][1]["range"] =
            serde_json::json!({"value": 42., "min":null, "max":null, "step":null});
        assert!(serde_json::from_value::<AccessibilityTree>(forged).is_err());
    }
}
