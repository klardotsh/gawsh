use sled::IVec;

pub fn concatenate_merge(
    _key: &[u8],              // the key being merged
    old_value: Option<&[u8]>, // the previous value, if one existed
    merged_bytes: &[u8],      // the new bytes being merged in
) -> Option<Vec<u8>> {
    // set the new value, return None to delete
    let mut ret = old_value.map(|ov| ov.to_vec()).unwrap_or_else(Vec::new);

    for fname in ret.split(|c| c == &0) {
        if fname == merged_bytes {
            return Some(ret);
        }
    }

    if ret.len() > 0 {
        ret.push(0);
    }

    ret.extend_from_slice(merged_bytes);

    Some(ret)
}
