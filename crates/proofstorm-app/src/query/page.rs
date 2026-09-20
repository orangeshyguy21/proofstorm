//! Admit one item against its complete response, restoring the page on rejection.

/// Tentatively append an item and its continuation, then measure the complete
/// response. Return false if it does not fit; restore both fields on rejection
/// or measurement error. The caller owns first-item errors and scan progress.
///
/// `fields` selects the page's item vector and continuation. `fits` includes
/// the transport envelope, any secondary limit, and reserves for later metadata.
pub fn push_bounded<P, T, E>(
    page: &mut P,
    item: T,
    next_cursor: Option<String>,
    fields: fn(&mut P) -> (&mut Vec<T>, &mut Option<String>),
    fits: impl FnOnce(&P) -> Result<bool, E>,
) -> Result<bool, E> {
    let (items, cursor) = fields(page);
    items.push(item);
    let previous_cursor = std::mem::replace(cursor, next_cursor);
    let accepted = fits(page);
    if !matches!(accepted, Ok(true)) {
        let (items, cursor) = fields(page);
        items.pop();
        *cursor = previous_cursor;
    }
    accepted
}

#[cfg(test)]
mod tests;
