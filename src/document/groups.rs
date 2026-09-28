//! Layer groups. The stack stays one flat list, bottom to top. A group sits
//! directly above everything inside it, and each layer names the group it is
//! in, so a group and its contents always form one unbroken run: its block.

use super::{blank_layer, Document, Error, Layer, LayerKind, PixelRect};
use std::collections::BTreeSet;
use std::ops::Range;

impl Document {
    pub fn is_group(&self, index: usize) -> bool {
        self.layers
            .get(index)
            .is_some_and(|layer| matches!(layer.kind, LayerKind::Group { .. }))
    }

    /// The index of the group the layer is in, if any.
    pub fn parent_index(&self, index: usize) -> Option<usize> {
        let parent = self.layers.get(index)?.parent?;
        self.layers.iter().position(|layer| layer.id == parent)
    }

    /// How many groups the layer is inside.
    pub fn depth(&self, index: usize) -> usize {
        let mut depth = 0;
        let mut at = index;
        while let Some(parent) = self.parent_index(at) {
            depth += 1;
            at = parent;
        }
        depth
    }

    /// Whether the layer at `index` is inside the group with id `group`, at
    /// any depth.
    fn is_inside(&self, index: usize, group: u64) -> bool {
        let mut at = index;
        while let Some(parent) = self.parent_index(at) {
            if self.layers[parent].id == group {
                return true;
            }
            at = parent;
        }
        false
    }

    /// The layer and, for a group, everything inside it: a run of indices
    /// ending at `index`.
    pub fn block(&self, index: usize) -> Range<usize> {
        let Some(layer) = self.layers.get(index) else {
            return index..index;
        };
        let mut start = index;
        if matches!(layer.kind, LayerKind::Group { .. }) {
            while start > 0 && self.is_inside(start - 1, layer.id) {
                start -= 1;
            }
        }
        start..index + 1
    }

    /// Whether the layer and every group around it are visible.
    pub fn shown(&self, index: usize) -> bool {
        let mut at = index;
        loop {
            if !self.layers[at].visible {
                return false;
            }
            match self.parent_index(at) {
                Some(parent) => at = parent,
                None => return true,
            }
        }
    }

    /// Whether a group around the layer is collapsed in the layers panel.
    pub fn hidden_in_panel(&self, index: usize) -> bool {
        let mut at = index;
        while let Some(parent) = self.parent_index(at) {
            if matches!(
                self.layers[parent].kind,
                LayerKind::Group { collapsed: true }
            ) {
                return true;
            }
            at = parent;
        }
        false
    }

    /// `indices` with the contents of any groups among them, in stack order.
    pub fn with_contents(&self, indices: &[usize]) -> Vec<usize> {
        let mut all = BTreeSet::new();
        for &index in indices {
            all.extend(self.block(index));
        }
        all.into_iter().collect()
    }

    /// The box around what a layer draws, or for a group, what its contents
    /// draw. `None` when that is nothing.
    pub fn item_bounds(&self, index: usize) -> Option<PixelRect> {
        self.block(index)
            .filter_map(|at| self.layers[at].content_bounds())
            .reduce(|a, b| {
                let left = a.x.min(b.x);
                let top = a.y.min(b.y);
                let right = (a.x + a.width as i32).max(b.x + b.width as i32);
                let bottom = (a.y + a.height as i32).max(b.y + b.height as i32);
                PixelRect {
                    x: left,
                    y: top,
                    width: (right - left) as u32,
                    height: (bottom - top) as u32,
                }
            })
    }

    /// The next layer or group above or below at the same level: what raise
    /// and lower trade places with.
    pub fn sibling(&self, index: usize, above: bool) -> Option<usize> {
        let parent = self.layers.get(index)?.parent;
        if above {
            // The block above starts right after this layer. Its top is the
            // first layer at this level whose block starts there.
            let next = index + 1;
            return (next..self.layers.len())
                .find(|&top| self.layers[top].parent == parent && self.block(top).start == next);
        }
        let below = self.block(index).start.checked_sub(1)?;
        (self.layers[below].parent == parent).then_some(below)
    }
}

#[cfg(test)]
mod tests {
    use crate::document::{
        composite, Background, BlendMode, Command, Document, Editor, Error, LayerKind, NewCanvas,
    };
    use image::{Rgba, RgbaImage};

    /// A 4×1 canvas with one solid layer per color, bottom to top, each
    /// covering the whole canvas and named after its position.
    fn stack(colors: &[[u8; 4]]) -> Editor {
        let mut editor = Editor::new(
            Document::new(NewCanvas {
                width: 4,
                height: 1,
                ppi: 72.0,
                background: Background::Transparent,
            })
            .unwrap(),
        );
        editor
            .apply(Command::DeleteLayers { indices: vec![] })
            .unwrap();
        for (i, &color) in colors.iter().enumerate() {
            editor
                .apply(Command::AddImageLayer {
                    name: format!("L{i}"),
                    image: RgbaImage::from_pixel(4, 1, Rgba(color)),
                })
                .unwrap();
        }
        editor
            .apply(Command::DeleteLayers { indices: vec![0] })
            .unwrap();
        editor
    }

    fn names(editor: &Editor) -> Vec<String> {
        editor
            .document()
            .layers()
            .iter()
            .map(|layer| layer.name.clone())
            .collect()
    }

    const RED: [u8; 4] = [255, 0, 0, 255];
    const GREEN: [u8; 4] = [0, 255, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];

    #[test]
    fn grouping_gathers_layers_under_a_new_group_and_undoes() {
        let mut editor = stack(&[RED, GREEN, BLUE]);
        editor
            .apply(Command::Group {
                indices: vec![0, 2],
            })
            .unwrap();
        assert_eq!(names(&editor), ["L1", "L0", "L2", "Group 1"]);
        let doc = editor.document();
        assert!(doc.is_group(3));
        assert_eq!(doc.block(3), 1..4);
        assert_eq!(doc.depth(1), 1);
        assert_eq!(doc.depth(0), 0);
        assert_eq!(doc.selected_indices(), [3]);
        editor.undo();
        assert_eq!(names(&editor), ["L0", "L1", "L2"]);
    }

    #[test]
    fn a_hidden_group_hides_its_contents() {
        let mut editor = stack(&[RED, GREEN]);
        editor.apply(Command::Group { indices: vec![1] }).unwrap();
        editor
            .apply(Command::SetVisibility {
                index: 2,
                visible: false,
            })
            .unwrap();
        let doc = editor.document();
        assert_eq!(composite(doc).get_pixel(0, 0).0, RED);
        assert_eq!(doc.layer_at(0.5, 0.5), Some(0));
        assert!(!doc.shown(1));
    }

    #[test]
    fn a_faded_group_flattens_its_contents_first() {
        let mut editor = stack(&[BLUE, RED, GREEN]);
        editor
            .apply(Command::Group {
                indices: vec![1, 2],
            })
            .unwrap();
        editor
            .apply(Command::SetOpacity {
                index: 3,
                opacity: 0.5,
            })
            .unwrap();
        // The group looks like its top layer, green, at half strength over
        // blue. Fading each layer on its own would let red show through.
        let pixel = composite(editor.document()).get_pixel(0, 0).0;
        assert_eq!(pixel[0], 0, "no red: {pixel:?}");
        assert!(
            pixel[1].abs_diff(128) <= 1 && pixel[2].abs_diff(127) <= 1,
            "{pixel:?}"
        );

        // At full opacity in Normal mode, a Multiply layer inside still
        // mixes with the blue below the group.
        editor
            .apply(Command::SetOpacity {
                index: 3,
                opacity: 1.0,
            })
            .unwrap();
        editor
            .apply(Command::SetBlend {
                index: 2,
                blend: BlendMode::Multiply,
            })
            .unwrap();
        editor
            .apply(Command::SetVisibility {
                index: 1,
                visible: false,
            })
            .unwrap();
        assert_eq!(
            composite(editor.document()).get_pixel(0, 0).0,
            [0, 0, 0, 255]
        );
    }

    #[test]
    fn reordering_moves_whole_groups_and_joins_the_target_group() {
        let mut editor = stack(&[RED, GREEN, BLUE]);
        editor
            .apply(Command::Group {
                indices: vec![1, 2],
            })
            .unwrap();
        // [L0, L1, L2, G]. Raising L0 passes the whole group.
        let doc = editor.document();
        assert_eq!(doc.sibling(0, true), Some(3));
        assert_eq!(doc.sibling(1, true), Some(2));
        assert_eq!(
            doc.sibling(2, true),
            None,
            "the top of a group has no sibling above"
        );
        editor.apply(Command::Reorder { from: 0, to: 3 }).unwrap();
        assert_eq!(names(&editor), ["L1", "L2", "Group 1", "L0"]);
        // Dropping it back beside a layer in the group puts it in the group.
        editor.apply(Command::Reorder { from: 3, to: 1 }).unwrap();
        assert_eq!(names(&editor), ["L1", "L0", "L2", "Group 1"]);
        assert_eq!(editor.document().parent_index(1), Some(3));
        // A group can't move inside itself.
        assert!(!editor.apply(Command::Reorder { from: 3, to: 1 }).unwrap());
        assert!(matches!(
            editor.apply(Command::MoveIntoGroup { index: 3, group: 3 }),
            Err(Error::WrongKind)
        ));
    }

    #[test]
    fn moving_duplicating_and_deleting_a_group_takes_its_contents() {
        let mut editor = stack(&[RED, GREEN, BLUE]);
        editor
            .apply(Command::Group {
                indices: vec![1, 2],
            })
            .unwrap();
        editor
            .apply(Command::MoveLayers {
                indices: vec![3],
                dx: 2,
                dy: 0,
            })
            .unwrap();
        let x: Vec<i32> = editor
            .document()
            .layers()
            .iter()
            .map(|layer| layer.x)
            .collect();
        assert_eq!(x, [0, 2, 2, 0]);

        editor.apply(Command::DuplicateLayer { index: 3 }).unwrap();
        assert_eq!(
            names(&editor),
            ["L0", "L1", "L2", "Group 1", "L1", "L2", "Group 1 copy"]
        );
        let doc = editor.document();
        assert_eq!(doc.block(6), 4..7);
        assert_eq!(doc.parent_index(4), Some(6), "copies join the copied group");

        editor
            .apply(Command::DeleteLayers { indices: vec![3] })
            .unwrap();
        assert_eq!(names(&editor), ["L0", "L1", "L2", "Group 1 copy"]);
    }

    #[test]
    fn ungrouping_leaves_the_contents_in_place_and_selected() {
        let mut editor = stack(&[RED, GREEN, BLUE]);
        editor
            .apply(Command::Group {
                indices: vec![1, 2],
            })
            .unwrap();
        editor
            .apply(Command::MoveIntoGroup { index: 0, group: 3 })
            .unwrap();
        assert_eq!(names(&editor), ["L1", "L2", "L0", "Group 1"]);
        editor.apply(Command::Ungroup { index: 3 }).unwrap();
        assert_eq!(names(&editor), ["L1", "L2", "L0"]);
        let doc = editor.document();
        assert!(doc.layers().iter().all(|layer| layer.parent.is_none()));
        assert_eq!(doc.selected_indices(), [0, 1, 2]);
        assert!(matches!(doc.layers()[2].kind, LayerKind::Raster));
    }

    #[test]
    fn a_group_refuses_pixel_edits_and_can_collapse() {
        let mut editor = stack(&[RED]);
        editor.apply(Command::Group { indices: vec![0] }).unwrap();
        assert!(matches!(
            editor.apply(Command::RotateLayer {
                index: 1,
                degrees_cw: 90.0,
            }),
            Err(Error::WrongKind)
        ));
        editor.set_collapsed(1, true).unwrap();
        assert!(editor.document().hidden_in_panel(0));
        assert!(editor.set_collapsed(0, true).is_err());
    }
}

/// Put `from`, with everything inside it, where `to` is, as a neighbor of
/// `to` in the same group. Moving up lands it above `to`, and moving down
/// lands it below, as dragging a row does.
pub(super) fn reorder(doc: &mut Document, from: usize, to: usize) -> Result<(), Error> {
    if from >= doc.layers.len() || to >= doc.layers.len() {
        return Err(Error::BadLayer);
    }
    let moving = doc.block(from);
    if moving.contains(&to) {
        return Ok(());
    }
    let target = doc.layers[to].id;
    let parent = doc.layers[to].parent;
    let upward = to > from;
    let mut block: Vec<Layer> = doc.layers.drain(moving).collect();
    block
        .last_mut()
        .expect("a block holds its own layer")
        .parent = parent;
    let to = doc
        .layers
        .iter()
        .position(|layer| layer.id == target)
        .expect("the target stays in the stack");
    let place = doc.block(to);
    let at = if upward { place.end } else { place.start };
    doc.layers.splice(at..at, block);
    Ok(())
}

/// Move `index`, with everything inside it, to the top of `group`.
pub(super) fn move_into(doc: &mut Document, index: usize, group: usize) -> Result<(), Error> {
    if index >= doc.layers.len() || !doc.is_group(group) {
        return Err(Error::BadLayer);
    }
    let moving = doc.block(index);
    if moving.contains(&group) {
        return Err(Error::WrongKind);
    }
    let group_id = doc.layers[group].id;
    let mut block: Vec<Layer> = doc.layers.drain(moving).collect();
    block
        .last_mut()
        .expect("a block holds its own layer")
        .parent = Some(group_id);
    let group = doc
        .layers
        .iter()
        .position(|layer| layer.id == group_id)
        .expect("the group stays in the stack");
    doc.layers.splice(group..group, block);
    Ok(())
}

/// Gather `indices` into a new group, where the topmost of them was, and
/// select it.
pub(super) fn group(doc: &mut Document, indices: &[usize]) -> Result<(), Error> {
    if indices.is_empty() || indices.iter().any(|&index| index >= doc.layers.len()) {
        return Err(Error::BadLayer);
    }
    let members = doc.with_contents(indices);
    let top = *members.last().expect("at least one layer");
    // The new group joins whatever held the topmost layer, unless that holder
    // is being gathered too.
    let mut holder = top;
    while let Some(parent) = doc.parent_index(holder) {
        if !members.contains(&parent) {
            break;
        }
        holder = parent;
    }
    let parent = doc.layers[holder].parent;
    let ids: BTreeSet<u64> = members.iter().map(|&index| doc.layers[index].id).collect();
    let at = (0..top).filter(|index| !members.contains(index)).count();

    let count = doc
        .layers
        .iter()
        .filter(|layer| matches!(layer.kind, LayerKind::Group { .. }))
        .count();
    let mut group = blank_layer(doc, &format!("Group {}", count + 1));
    group.pixels = image::RgbaImage::new(1, 1);
    group.kind = LayerKind::Group { collapsed: false };
    group.parent = parent;
    let group_id = group.id;

    let layers = std::mem::take(&mut doc.layers);
    let (mut gathered, rest): (Vec<Layer>, Vec<Layer>) = layers
        .into_iter()
        .partition(|layer| ids.contains(&layer.id));
    for layer in &mut gathered {
        if layer.parent.is_none_or(|parent| !ids.contains(&parent)) {
            layer.parent = Some(group_id);
        }
    }
    gathered.push(group);
    let mut rest = rest;
    rest.splice(at..at, gathered);
    doc.layers = rest;
    doc.selection = BTreeSet::from([group_id]);
    Ok(())
}

/// Take a group apart, leaving its contents where they are, selected.
pub(super) fn ungroup(doc: &mut Document, index: usize) -> Result<(), Error> {
    if !doc.is_group(index) {
        return Err(Error::WrongKind);
    }
    let group = doc.layers.remove(index);
    let mut freed = BTreeSet::new();
    for layer in &mut doc.layers {
        if layer.parent == Some(group.id) {
            layer.parent = group.parent;
            freed.insert(layer.id);
        }
    }
    doc.selection = freed;
    Ok(())
}

/// A copy of `index`, with everything inside it, right above the original,
/// offset so it can be told apart.
pub(super) fn duplicate(doc: &mut Document, index: usize) -> Result<(), Error> {
    if index >= doc.layers.len() {
        return Err(Error::BadLayer);
    }
    let block = doc.block(index);
    let originals: Vec<Layer> = doc.layers[block.clone()].to_vec();
    let mut renamed = std::collections::HashMap::new();
    let mut copies = Vec::with_capacity(originals.len());
    for original in &originals {
        let mut copy = original.clone();
        copy.id = doc.next_id;
        doc.next_id += 1;
        renamed.insert(original.id, copy.id);
        if !matches!(copy.kind, LayerKind::Group { .. }) {
            copy.x += 16;
            copy.y += 16;
        }
        copies.push(copy);
    }
    for copy in &mut copies {
        if let Some(parent) = copy.parent {
            copy.parent = renamed.get(&parent).copied().or(Some(parent));
        }
    }
    let top = copies.last_mut().expect("a block holds its own layer");
    top.name = format!("{} copy", top.name);
    let top_id = top.id;
    doc.layers.splice(block.end..block.end, copies);
    doc.selection = BTreeSet::from([top_id]);
    Ok(())
}
