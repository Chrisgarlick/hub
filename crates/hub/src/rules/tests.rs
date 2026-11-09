use super::*;

const DOC: &str = r#"---
title: Writing rules
type: writing-rules
---

# Writing rules

What must, mustn't or should appear.

## Everywhere

- Don't mention the "2024 data breach" or a "data leak".
- Always sign off as "The Acme team".
- Prefer "car park" to "parking lot".
- Avoid exclamation marks: `/!+/`
- Keep sentences short.
- 

## Reports and proposals

- Never quote prices without VAT.
- Don't say "Acme" in lower case (case-sensitive) [design, typeset]

```
- Not a rule: it's in a code block.
```
"#;

#[test]
fn rules_from_prose() {
    let rules = parse(DOC);
    let texts: Vec<&str> = rules.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(texts.len(), 7, "{texts:#?}");

    let breach = &rules[0];
    assert_eq!(
        (breach.kind, breach.anchor.as_deref()),
        (RuleKind::MustNot, Some("everywhere"))
    );
    assert_eq!(breach.applies_to, ["all"]);
    assert_eq!(
        breach.check.as_ref().unwrap().terms,
        ["2024 data breach", "data leak"]
    );

    let sign = &rules[1];
    assert_eq!(sign.kind, RuleKind::Must);
    assert_eq!(
        sign.check.as_ref().unwrap().required_terms,
        ["The Acme team"]
    );

    let park = &rules[2];
    assert_eq!(park.kind, RuleKind::Prefer);
    assert_eq!(
        park.check.as_ref().unwrap().terms,
        ["parking lot"],
        "only what to avoid"
    );

    let bang = &rules[3];
    assert_eq!(
        (bang.kind, bang.check.as_ref().unwrap().pattern.as_deref()),
        (RuleKind::Avoid, Some("!+"))
    );

    let short = &rules[4];
    assert_eq!(
        (short.kind, short.check.is_none()),
        (RuleKind::Prefer, true),
        "guidance"
    );

    let vat = &rules[5];
    assert_eq!(
        (vat.kind, vat.applies_to.as_slice()),
        (RuleKind::MustNot, ["typeset".to_string()].as_slice())
    );
    assert!(vat.check.is_none(), "no quotes, so Claude judges it");

    let case = &rules[6];
    assert_eq!(case.applies_to, ["design", "typeset"]);
    assert_eq!(case.text, "Don't say \"Acme\" in lower case");
    assert!(case.check.as_ref().unwrap().case_sensitive);
}

#[test]
fn bad_patterns_become_warnings() {
    let rules = parse("## Copy\n\n- Avoid `/(unclosed/`\n");
    assert_eq!(rules[0].applies_to, ["design"]);
    assert!(rules[0].check.is_none());
    assert!(rules[0].warning.as_deref().unwrap().contains("isn't valid"));
}

#[test]
fn whole_words_and_case() {
    let check = Check {
        terms: vec!["data leak".into(), "C++".into()],
        ..Check::default()
    };
    let text = "No data\nleaks here. A data leak there, and DATA LEAK.\nWe use C++ too.";
    let found = run(&check, text).unwrap();
    let hits: Vec<(&str, usize, usize)> = found
        .spans
        .iter()
        .map(|s| (s.text.as_str(), s.line, s.column))
        .collect();
    assert_eq!(
        hits,
        [("data leak", 2, 15), ("DATA LEAK", 2, 36), ("C++", 3, 8)]
    );
    assert!(
        run(&check, "metadata leakage").is_none(),
        "not inside other words"
    );

    let exact = Check {
        terms: vec!["acme".into()],
        case_sensitive: true,
        ..Check::default()
    };
    assert!(run(&exact, "Acme and ACME").is_none());
    assert_eq!(run(&exact, "Acme, acme").unwrap().spans[0].column, 7);
}

#[test]
fn required_terms_and_patterns() {
    let must = Check {
        required_terms: vec!["The Acme team".into()],
        ..Check::default()
    };
    assert_eq!(
        run(&must, "Thanks, Acme").unwrap().missing,
        ["The Acme team"]
    );
    assert!(run(&must, "Best,\nthe acme team").is_none());

    let bang = Check {
        pattern: Some("!+".into()),
        ..Check::default()
    };
    let found = run(&bang, "Wow!! Great!").unwrap();
    assert_eq!(
        found
            .spans
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>(),
        ["!!", "!"]
    );
}

#[test]
fn kinds_from_first_words() {
    for (t, k) in [
        ("Don’t use jargon", RuleKind::MustNot),
        ("do not name staff", RuleKind::MustNot),
        ("**Never** promise dates", RuleKind::MustNot),
        ("Must include the address", RuleKind::Must),
        ("Try not to use passive voice", RuleKind::Avoid),
        ("Use British spelling", RuleKind::Prefer),
        ("Donkeys are fine", RuleKind::Prefer),
    ] {
        assert_eq!(kind_of(t), k, "{t}");
    }
}
