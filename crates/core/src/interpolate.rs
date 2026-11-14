#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vars {
    pub sha: String,
    pub reference: String,
    pub version: String,
    pub run: u64,
}

impl Vars {
    pub fn placeholder() -> Self {
        Self { sha: "0".repeat(40), reference: "main".to_owned(), version: "0.0.0-dev".to_owned(), run: 0 }
    }

    fn lookup(&self, name: &str) -> Option<String> {
        match name {
            "sha" => Some(self.sha.clone()),
            "short_sha" => Some(self.sha.chars().take(12).collect()),
            "ref" => Some(self.reference.clone()),
            "version" => Some(self.version.clone()),
            "run" => Some(self.run.to_string()),
            _ => None,
        }
    }
}

pub fn interpolate(template: &str, vars: &Vars) -> Result<String, String> {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('$') {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        if let Some(escaped) = tail.strip_prefix("$${") {
            out.push_str("${");
            rest = escaped;
        } else if let Some(open) = tail.strip_prefix("${") {
            let end = open.find('}').ok_or_else(|| format!("unclosed \"${{\" in {template:?}"))?;
            let name = &open[..end];
            let value = vars
                .lookup(name)
                .ok_or_else(|| format!("unknown variable ${{{name}}}; use sha, short_sha, ref, version or run"))?;
            out.push_str(&value);
            rest = &open[end + 1..];
        } else {
            out.push('$');
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitutes_known_variables_and_keeps_escapes() {
        let vars = Vars { sha: "a".repeat(40), reference: "main".into(), version: "1.2.3".into(), run: 7 };
        assert_eq!(
            interpolate("${short_sha}-${version}-$${HOME}-$PATH-${run}", &vars).unwrap(),
            format!("{}-1.2.3-${{HOME}}-$PATH-7", "a".repeat(12))
        );
        assert!(interpolate("${secret}", &vars).unwrap_err().contains("unknown variable"));
    }
}
