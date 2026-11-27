//! 自然拼音混排排序键的单元测试。
//!
//! 这些用例直接对应 Python 版 `natural_sort_key` 的行为,
//! 由 `tools/compare_impl.py` 做端到端对照, 这里做细粒度定位。

use desktop_icon_arranger::sorting::{KeyPart, natural_sort_key};

/// 把排序键渲染成可读形式 (与 Python 的 `(type, num, text)` 三元组对应)。
fn render(s: &str) -> Vec<String> {
    natural_sort_key(s)
        .0
        .iter()
        .map(|p| match p {
            KeyPart::Text(t) => format!("T:{t}"),
            KeyPart::Number(n) => format!("N:{n}"),
        })
        .collect()
}

#[test]
fn 纯英文整段是一个元素() {
    // Python: _lazy_pinyin("dsh-startup") -> ["dsh-startup"]
    assert_eq!(render("dsh-startup"), vec!["T:dsh-startup"]);
}

#[test]
fn 汉字逐字转拼音() {
    // Python: _lazy_pinyin("钉钉") -> ["ding", "ding"]
    assert_eq!(render("钉钉"), vec!["T:ding", "T:ding"]);
}

#[test]
fn 汉字与非汉字混合() {
    // Python: _lazy_pinyin("个人 - Edge") -> ["ge", "ren", " - edge"]
    assert_eq!(render("个人 - Edge"), vec!["T:ge", "T:ren", "T: - edge"]);
}

#[test]
fn 带扩展名的文档() {
    // Python: _lazy_pinyin("电脑配置.docx") -> ["dian","nao","pei","zhi",".docx"]
    assert_eq!(
        render("电脑配置.docx"),
        vec!["T:dian", "T:nao", "T:pei", "T:zhi", "T:.docx"]
    );
}

#[test]
fn 空格算非汉字段() {
    // Python: _lazy_pinyin("新建 XLSX 工作表.xlsx") -> ["xin","jian"," xlsx ","gong","zuo","biao",".xlsx"]
    assert_eq!(
        render("新建 XLSX 工作表.xlsx"),
        vec![
            "T:xin", "T:jian", "T: xlsx ", "T:gong", "T:zuo", "T:biao", "T:.xlsx"
        ]
    );
}

#[test]
fn 数字段按数值比较() {
    // "项目2" 排在 "项目10" 之前
    let k2 = natural_sort_key("项目2");
    let k10 = natural_sort_key("项目10");
    assert!(k2.cmp_key(&k10) == std::cmp::Ordering::Less);

    // "Navicat 2" 排在 "Navicat 17" 之前
    let a = natural_sort_key("Navicat 2");
    let b = natural_sort_key("Navicat 17");
    assert!(a.cmp_key(&b) == std::cmp::Ordering::Less);
}

#[test]
fn 数字段与文本段的相对次序() {
    // Python 的元组比较: (1, num, "") vs (0, 0, text) -> 数字段永远在文本段之后
    let k_num = natural_sort_key("2abc");
    let k_txt = natural_sort_key("abc");
    assert_eq!(k_num.0.len(), 2);
    assert_eq!(k_txt.0.len(), 1);
    assert_eq!(k_num.0[0], KeyPart::Number(2));
    assert_eq!(k_txt.0[0], KeyPart::Text("abc".into()));
    // 数字段 (type=1) 排在文本段 (type=0) 之后
    assert_eq!(k_txt.cmp_key(&k_num), std::cmp::Ordering::Less);
}

#[test]
fn 前缀相同时短者在前() {
    let a = natural_sort_key("PixPin");
    let b = natural_sort_key("PixPin Pro");
    assert_eq!(a.cmp_key(&b), std::cmp::Ordering::Less);
}

#[test]
fn 英文与汉字同台混排() {
    // 千问(Q) 排在 PixPin(P) 之后、ToDesk(T) 之前
    let q = natural_sort_key("千问");
    let p = natural_sort_key("PixPin");
    let t = natural_sort_key("ToDesk");
    assert_eq!(q.0[0], KeyPart::Text("qian".into()));
    assert!(p.cmp_key(&q) == std::cmp::Ordering::Less);
    assert!(q.cmp_key(&t) == std::cmp::Ordering::Less);
}

#[test]
fn 大小写不敏感() {
    let a = natural_sort_key("Everything");
    let b = natural_sort_key("everything");
    assert_eq!(a.cmp_key(&b), std::cmp::Ordering::Equal);
}
