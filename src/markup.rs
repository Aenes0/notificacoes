//! Marcação de corpo (subconjunto HTML da spec de notificações) → texto puro.

/// Tira tags, decodifica entidades e apara. Um `<` que não abre uma tag
/// (não seguido de letra ou `/`, ou sem `>` depois) fica como texto.
pub fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find(['<', '&']) {
        out.push_str(&rest[..pos]);
        let tail = &rest[pos..];
        let consumed = if tail.starts_with('<') {
            match tag_len(tail) {
                Some(len) => len,
                None => {
                    out.push('<');
                    1
                }
            }
        } else {
            match entity(tail) {
                Some((ch, len)) => {
                    out.push(ch);
                    len
                }
                None => {
                    out.push('&');
                    1
                }
            }
        };
        rest = &tail[consumed..];
    }
    out.push_str(rest);
    out.trim().to_string()
}

/// Comprimento da tag que começa em `s` (que começa com `<`), se for uma tag.
fn tag_len(s: &str) -> Option<usize> {
    let next = s[1..].chars().next()?;
    if !(next.is_ascii_alphabetic() || next == '/') {
        return None;
    }
    s.find('>').map(|i| i + 1)
}

/// Entidade no início de `s` (que começa com `&`): caractere e comprimento.
fn entity(s: &str) -> Option<(char, usize)> {
    let end = s.char_indices().take(12).find(|&(_, c)| c == ';')?.0;
    let name = &s[1..end];
    let ch = match name {
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "amp" => '&',
        _ => {
            let num = name.strip_prefix('#')?;
            let code = match num.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => num.parse::<u32>().ok()?,
            };
            char::from_u32(code)?
        }
    };
    Some((ch, end + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_stripped_and_entities_decoded() {
        assert_eq!(plain("Pronto: <a href=\"x\">site</a> &amp; <b>mais</b>"), "Pronto: site & mais");
    }

    #[test]
    fn numeric_entities_decoded() {
        assert_eq!(plain("&#233;&#xE9;"), "éé");
    }

    #[test]
    fn img_tag_removed() {
        assert_eq!(plain("a<img src=\"x\"/>b"), "ab");
    }

    #[test]
    fn lone_less_than_is_kept() {
        assert_eq!(plain("2 < 3 e 5 > 4"), "2 < 3 e 5 > 4");
    }
}
