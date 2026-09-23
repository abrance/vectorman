//! 从 `/sys/kernel/btf/vmlinux` 解析 `struct` 成员字节偏移。
//!
//! 为什么自己做：内核态要用 `bpf_probe_read_kernel` 从 `struct sock` 里挖连接键，字段偏移
//! 与内核版本绑定，**不能硬编码**。aya 的用户态 crate 没有 CO-RE 字段重定位，`aya-obj` 的
//! BTF 公开 API 也取不到结构体成员（成员字段是 `pub(crate)`），而 BTF 格式本身很小：
//! 一个头 + 类型段 + 字符串段。这里只解析需要的那部分。
//!
//! 解析结果由 [`crate::cfg`] 写进 `CFG` map 交给内核态使用。

use std::collections::HashMap;

/// BTF 魔数（大端 `0xeb9f`）。
const BTF_MAGIC: u16 = 0xeb9f;

const KIND_INT: u32 = 1;
const KIND_PTR: u32 = 2;
const KIND_ARRAY: u32 = 3;
const KIND_STRUCT: u32 = 4;
const KIND_UNION: u32 = 5;
const KIND_ENUM: u32 = 6;
const KIND_FWD: u32 = 7;
const KIND_TYPEDEF: u32 = 8;
const KIND_VOLATILE: u32 = 9;
const KIND_CONST: u32 = 10;
const KIND_RESTRICT: u32 = 11;
const KIND_FUNC: u32 = 12;
const KIND_FUNC_PROTO: u32 = 13;
const KIND_VAR: u32 = 14;
const KIND_DATASEC: u32 = 15;
const KIND_FLOAT: u32 = 16;
const KIND_DECL_TAG: u32 = 17;
const KIND_TYPE_TAG: u32 = 18;
const KIND_ENUM64: u32 = 19;

/// 一个结构体成员：名字 + 位偏移 + 位宽。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub name: String,
    /// BTF 里是**位**偏移，这里换算成字节（非 8 的倍数按向下取整处理并保留原始位偏移）。
    pub offset_bits: u32,
    pub bit_size: u32,
    /// 成员类型 id（匿名 union/struct 需要递归下去找）。
    pub type_id: u32,
}

impl Member {
    /// 字节偏移（`bpf_probe_read_kernel` 用这个值）。
    #[must_use]
    pub fn offset_bytes(&self) -> u32 {
        self.offset_bits / 8
    }
}

/// 解析出的 BTF：类型 id → 类型，名字 → 类型 id。
#[derive(Debug, Default)]
pub struct Btf {
    types: Vec<TypeRecord>,
    by_name: HashMap<(String, u32), u32>,
}

#[derive(Debug, Clone, Default)]
struct TypeRecord {
    /// 类型名（成员查找用不到，保留便于排障）。
    #[allow(dead_code)]
    name: String,
    kind: u32,
    /// struct/union 的成员、enum 的枚举项等「变长」部分。
    members: Vec<Member>,
    /// 成员位域编码（`kind_flag`）：此时成员的 `offset` 字段高 8 位是位宽。
    #[allow(dead_code)]
    kind_flag: bool,
    /// 指针/typedef/const 等的目标类型 id。
    referred_id: u32,
    /// 类型大小（字节）。
    size: u32,
}

impl Btf {
    /// 解析 BTF blob。
    pub fn parse(data: &[u8]) -> Result<Self, String> {
        let reader = Reader::new(data);
        let magic = reader.u16(0).ok_or("BTF 太短")?;
        if magic != BTF_MAGIC {
            return Err(format!("BTF 魔数不符：0x{magic:04x}"));
        }
        let hdr_len = reader.u32(4).ok_or("BTF 头截断")? as usize;
        let type_off = reader.u32(8).ok_or("BTF 头截断")? as usize;
        let type_len = reader.u32(12).ok_or("BTF 头截断")? as usize;
        let str_off = reader.u32(16).ok_or("BTF 头截断")? as usize;
        let str_len = reader.u32(20).ok_or("BTF 头截断")? as usize;

        let types_start = hdr_len.checked_add(type_off).ok_or("BTF type 段偏移溢出")?;
        let types_end = types_start
            .checked_add(type_len)
            .ok_or("BTF type 段长度溢出")?;
        let strs_start = hdr_len
            .checked_add(str_off)
            .ok_or("BTF string 段偏移溢出")?;
        let strs_end = strs_start
            .checked_add(str_len)
            .ok_or("BTF string 段长度溢出")?;
        if types_end > data.len() || strs_end > data.len() {
            return Err("BTF 段超出文件长度".to_string());
        }
        let types_data = &data[types_start..types_end];
        let strs = &data[strs_start..strs_end];

        let mut btf = Btf::default();
        // BTF 的类型 id 从 1 开始；索引 0 占位。
        btf.types.push(TypeRecord::default());
        let mut offset = 0usize;
        let tr = Reader::new(types_data);
        while offset + 12 <= types_data.len() {
            let name_off = tr.u32(offset).ok_or("类型记录截断")?;
            let info = tr.u32(offset + 4).ok_or("类型记录截断")?;
            let size_or_type = tr.u32(offset + 8).ok_or("类型记录截断")?;
            let kind = (info >> 24) & 0x1f;
            let vlen = (info & 0xffff) as usize;
            let kind_flag = (info >> 31) & 1 == 1;
            let name = string_at(strs, name_off);
            offset += 12;

            let mut record = TypeRecord {
                name: name.clone(),
                kind,
                kind_flag,
                size: size_or_type,
                ..TypeRecord::default()
            };
            match kind {
                KIND_STRUCT | KIND_UNION => {
                    for _ in 0..vlen {
                        let member_name_off = tr.u32(offset).ok_or("成员截断")?;
                        let member_type = tr.u32(offset + 4).ok_or("成员截断")?;
                        let member_offset = tr.u32(offset + 8).ok_or("成员截断")?;
                        let (offset_bits, bit_size) = if kind_flag {
                            // 位域：offset 低 24 位是位偏移，高 8 位是位宽。
                            (member_offset & 0x00ff_ffff, member_offset >> 24)
                        } else {
                            (member_offset, 0)
                        };
                        record.members.push(Member {
                            name: string_at(strs, member_name_off),
                            offset_bits,
                            bit_size,
                            type_id: member_type,
                        });
                        offset += 12;
                    }
                }
                KIND_INT => offset += 4,
                KIND_ENUM => offset += vlen * 8,
                KIND_ARRAY => offset += 12,
                KIND_FUNC_PROTO => offset += vlen * 8,
                KIND_VAR => offset += 4,
                KIND_DATASEC => offset += vlen * 12,
                KIND_DECL_TAG => offset += 4,
                KIND_ENUM64 => offset += vlen * 12,
                KIND_PTR | KIND_TYPEDEF | KIND_VOLATILE | KIND_CONST | KIND_RESTRICT
                | KIND_FUNC | KIND_FWD | KIND_FLOAT | KIND_TYPE_TAG => {
                    record.referred_id = size_or_type;
                }
                _ => return Err(format!("未知 BTF kind {kind}")),
            }
            let id = btf.types.len() as u32;
            if !name.is_empty() {
                btf.by_name.insert((name, kind), id);
            }
            btf.types.push(record);
        }
        Ok(btf)
    }

    /// 按名字与 kind 查类型 id。
    #[must_use]
    pub fn type_id(&self, name: &str, kind: u32) -> Option<u32> {
        self.by_name.get(&(name.to_string(), kind)).copied()
    }

    /// 结构体成员列表。
    #[must_use]
    pub fn struct_members(&self, name: &str) -> Option<&[Member]> {
        let id = self
            .type_id(name, KIND_STRUCT)
            .or_else(|| self.type_id(name, KIND_UNION))?;
        self.types.get(id as usize).map(|t| t.members.as_slice())
    }

    /// 成员的字节偏移。
    ///
    /// **会穿过匿名 struct/union 递归查找**：内核里 `sock_common.skc_daddr` 实际位于
    /// `union { __addrpair skc_addrpair; struct { ... }; }` 这个匿名 union 内，
    /// 直接看 `sock_common` 的成员列表只看得到一个无名成员。
    pub fn member_offset_bytes(&self, struct_name: &str, member: &str) -> Result<u32, String> {
        let id = self
            .type_id(struct_name, KIND_STRUCT)
            .or_else(|| self.type_id(struct_name, KIND_UNION))
            .ok_or_else(|| format!("BTF 里没有结构体 {struct_name}"))?;
        let found = self
            .find_member(id, member, 0, 0)
            .ok_or_else(|| format!("结构体 {struct_name} 没有成员 {member}"))?;
        Ok(found.offset_bytes())
    }

    /// 在 `type_id` 的成员里找 `member`，穿过匿名成员递归；`base_bits` 是当前累计位偏移。
    fn find_member(
        &self,
        type_id: u32,
        member: &str,
        base_bits: u32,
        depth: u32,
    ) -> Option<Member> {
        if depth > 8 {
            return None;
        }
        let record = self.resolved(type_id)?;
        if !matches!(record.kind, KIND_STRUCT | KIND_UNION) {
            return None;
        }
        for item in &record.members {
            let offset_bits = base_bits.saturating_add(item.offset_bits);
            if item.name == member {
                return Some(Member {
                    offset_bits,
                    ..item.clone()
                });
            }
            // 匿名成员（名字为空）可能是内层 struct/union，继续往下找。
            if item.name.is_empty() {
                if let Some(found) = self.find_member(item.type_id, member, offset_bits, depth + 1)
                {
                    return Some(found);
                }
            }
        }
        None
    }

    /// 穿过 typedef/const/volatile/restrict 找到真实类型记录。
    fn resolved(&self, mut id: u32) -> Option<&TypeRecord> {
        for _ in 0..16 {
            let record = self.types.get(id as usize)?;
            match record.kind {
                KIND_TYPEDEF | KIND_CONST | KIND_VOLATILE | KIND_RESTRICT => {
                    id = record.referred_id;
                }
                _ => return Some(record),
            }
        }
        None
    }

    /// 结构体大小（字节）。
    #[must_use]
    pub fn struct_size(&self, name: &str) -> Option<u32> {
        let id = self.type_id(name, KIND_STRUCT)?;
        self.types.get(id as usize).map(|t| t.size)
    }
}

fn string_at(strs: &[u8], offset: u32) -> String {
    let offset = offset as usize;
    if offset >= strs.len() {
        return String::new();
    }
    let end = strs[offset..]
        .iter()
        .position(|b| *b == 0)
        .map_or(strs.len(), |p| offset + p);
    String::from_utf8_lossy(&strs[offset..end]).into_owned()
}

/// 小端读取器（BTF 在小端机器上就是小端；大端内核的 vmlinux BTF 也是小端，因为 BTF 是
/// 编译器产物且 `bpf` 目标固定小端 —— 这里只支持小端，遇到大端内核直接报错而不是读错）。
struct Reader<'a> {
    data: &'a [u8],
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data }
    }

    fn u16(&self, offset: usize) -> Option<u16> {
        let bytes = self.data.get(offset..offset + 2)?;
        Some(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&self, offset: usize) -> Option<u32> {
        let bytes = self.data.get(offset..offset + 4)?;
        Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }
}

/// 从 vmlinux 文件读 BTF。
pub fn load_vmlinux_btf(path: &std::path::Path) -> Result<Btf, String> {
    let data = std::fs::read(path).map_err(|e| format!("读取 {} 失败：{e}", path.display()))?;
    Btf::parse(&data)
}

#[cfg(test)]
mod tests {
    use super::*;

    const VMLINUX: &str = "/sys/kernel/btf/vmlinux";

    #[test]
    fn struct_members_offset_bytes() {
        // 手写一个最小 BTF：一个 struct 两个成员（偏移 0 与 64 位）。
        let mut strs: Vec<u8> = vec![0];
        let name_off = |strs: &mut Vec<u8>, s: &str| -> u32 {
            let off = strs.len() as u32;
            strs.extend_from_slice(s.as_bytes());
            strs.push(0);
            off
        };
        let n_struct = name_off(&mut strs, "demo");
        let n_a = name_off(&mut strs, "a");
        let n_b = name_off(&mut strs, "b");

        let mut types: Vec<u8> = Vec::new();
        // type 1: struct demo { u32 a @0; u64 b @64; }
        types.extend_from_slice(&n_struct.to_le_bytes());
        let info: u32 = (KIND_STRUCT << 24) | 2; // vlen=2, kind_flag=0
        types.extend_from_slice(&info.to_le_bytes());
        types.extend_from_slice(&8u32.to_le_bytes());
        for (name, type_id, offset_bits) in [(n_a, 2u32, 0u32), (n_b, 3, 64)] {
            types.extend_from_slice(&name.to_le_bytes());
            types.extend_from_slice(&type_id.to_le_bytes());
            types.extend_from_slice(&offset_bits.to_le_bytes());
        }
        // 成员类型占位（int，各自 4 字节附加数据）
        for _ in 0..2 {
            types.extend_from_slice(&0u32.to_le_bytes()); // name_off
            types.extend_from_slice(&(KIND_INT << 24).to_le_bytes());
            types.extend_from_slice(&4u32.to_le_bytes());
            types.extend_from_slice(&32u32.to_le_bytes()); // int 的 encoding/size
        }

        let hdr_len = 24u32;
        let mut blob: Vec<u8> = Vec::new();
        blob.extend_from_slice(&BTF_MAGIC.to_le_bytes());
        blob.push(1); // version
        blob.push(0); // flags
        blob.extend_from_slice(&hdr_len.to_le_bytes());
        blob.extend_from_slice(&0u32.to_le_bytes()); // type_off（相对 hdr 之后）
        blob.extend_from_slice(&(types.len() as u32).to_le_bytes());
        blob.extend_from_slice(&(types.len() as u32).to_le_bytes()); // str_off：紧跟类型段
        blob.extend_from_slice(&(strs.len() as u32).to_le_bytes());
        blob.extend_from_slice(&types);
        blob.extend_from_slice(&strs);

        let btf = Btf::parse(&blob).expect("解析最小 BTF");
        assert_eq!(btf.member_offset_bytes("demo", "a").unwrap(), 0);
        assert_eq!(btf.member_offset_bytes("demo", "b").unwrap(), 8);
        assert_eq!(btf.struct_size("demo"), Some(8));
        assert!(btf.member_offset_bytes("demo", "c").is_err());
        assert!(btf.member_offset_bytes("nope", "a").is_err());
    }

    #[test]
    fn walks_anonymous_unions() {
        // struct outer { union { struct { u32 inner @0; }; }; }
        let mut strs: Vec<u8> = vec![0];
        let name_off = |strs: &mut Vec<u8>, s: &str| -> u32 {
            let off = strs.len() as u32;
            strs.extend_from_slice(s.as_bytes());
            strs.push(0);
            off
        };
        let n_outer = name_off(&mut strs, "outer");
        let n_inner = name_off(&mut strs, "inner");
        let n_anon = name_off(&mut strs, "");

        // 类型 1：外层 struct（一个匿名成员，指向类型 2）
        // 类型 2：匿名 union（一个匿名成员，指向类型 3）
        // 类型 3：内层 struct（成员 inner，偏移 32 位）
        let mut types: Vec<u8> = Vec::new();
        let push_member = |types: &mut Vec<u8>, name: u32, type_id: u32, offset_bits: u32| {
            types.extend_from_slice(&name.to_le_bytes());
            types.extend_from_slice(&type_id.to_le_bytes());
            types.extend_from_slice(&offset_bits.to_le_bytes());
        };
        // 1: struct outer
        types.extend_from_slice(&n_outer.to_le_bytes());
        types.extend_from_slice(&((KIND_STRUCT << 24) | 1).to_le_bytes());
        types.extend_from_slice(&4u32.to_le_bytes());
        push_member(&mut types, n_anon, 2, 0);
        // 2: union（匿名）
        types.extend_from_slice(&n_anon.to_le_bytes());
        types.extend_from_slice(&((KIND_UNION << 24) | 1).to_le_bytes());
        types.extend_from_slice(&4u32.to_le_bytes());
        push_member(&mut types, n_anon, 3, 0);
        // 3: struct（匿名，含 inner @32 位）
        types.extend_from_slice(&n_anon.to_le_bytes());
        types.extend_from_slice(&((KIND_STRUCT << 24) | 1).to_le_bytes());
        types.extend_from_slice(&4u32.to_le_bytes());
        push_member(&mut types, n_inner, 1, 32);

        let hdr_len = 24u32;
        let mut blob: Vec<u8> = Vec::new();
        blob.extend_from_slice(&BTF_MAGIC.to_le_bytes());
        blob.push(1);
        blob.push(0);
        blob.extend_from_slice(&hdr_len.to_le_bytes());
        blob.extend_from_slice(&0u32.to_le_bytes());
        blob.extend_from_slice(&(types.len() as u32).to_le_bytes());
        blob.extend_from_slice(&(types.len() as u32).to_le_bytes());
        blob.extend_from_slice(&(strs.len() as u32).to_le_bytes());
        blob.extend_from_slice(&types);
        blob.extend_from_slice(&strs);

        let btf = Btf::parse(&blob).unwrap();
        assert_eq!(btf.member_offset_bytes("outer", "inner").unwrap(), 4);
    }

    #[test]
    fn real_vmlinux_has_sock_layout() {
        let path = std::path::Path::new(VMLINUX);
        if !path.exists() {
            eprintln!("跳过：{VMLINUX} 不存在");
            return;
        }
        let btf = load_vmlinux_btf(path).expect("解析 vmlinux BTF");
        // 结构不变量（对所有 5.8+ 内核都成立）：
        // `skc_addrpair` 是 union，`skc_daddr` 必在 0、`skc_rcv_saddr` 紧随其后，
        // 且两者都在**匿名 union** 内 —— 直接看 `sock_common` 的成员列表只看得到一个无名成员。
        assert_eq!(
            btf.member_offset_bytes("sock_common", "skc_daddr").unwrap(),
            0
        );
        assert_eq!(
            btf.member_offset_bytes("sock_common", "skc_rcv_saddr")
                .unwrap(),
            4
        );
        // 6.1 的真实布局是 `__be16 skc_dport; __u16 skc_num;` 顺序排列（12 / 14），
        // **不是**同一个 union（本机 BTF 实测），所以这里只断言顺序与相对位置，
        // 不写具体数值 —— 具体数值由运行期解析得到，这正是内核态不硬编码偏移的意义。
        let dport = btf.member_offset_bytes("sock_common", "skc_dport").unwrap();
        let num = btf.member_offset_bytes("sock_common", "skc_num").unwrap();
        assert!(
            dport <= num,
            "skc_dport 不晚于 skc_num（dport={dport} num={num}）"
        );
        assert!(num - dport <= 2, "两者相邻（union 或顺序字段）");
        let family = btf
            .member_offset_bytes("sock_common", "skc_family")
            .unwrap();
        assert!(family > num, "skc_family 在两者之后");
        // `sk_protocol` 在 `struct sock` 里（不是 sock_common）。
        assert!(btf.member_offset_bytes("sock", "sk_protocol").is_ok());
        // `sock` 的第一个成员就是 `__sk_common`，所以 sock_common 的偏移可直接用在 sock* 上。
        let first = btf.struct_members("sock").unwrap().first().unwrap();
        assert_eq!(first.name, "__sk_common");
        assert_eq!(first.offset_bytes(), 0);
    }

    #[test]
    fn rejects_bad_magic() {
        assert!(Btf::parse(&[0u8; 32]).is_err());
        assert!(Btf::parse(&[]).is_err());
    }
}
