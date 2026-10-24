//! MDN pages for JavaScript built-ins and common Web APIs: `JSON.parse`, `arr.map`, `fetch`.
//! JavaScript has no local reference docs to read, so this is a small table; names that are
//! not in it get no page (they are most likely the project's own).

const JS: &str = "https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Global_Objects";
const API: &str = "https://developer.mozilla.org/en-US/docs/Web/API";

/// A docs page for a name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebDoc {
    pub title: String,
    pub url: String,
    /// `false`: a search page (the name fits several built-ins).
    pub exact: bool,
}

/// Global objects of the language (`Global_Objects/<name>`).
const GLOBAL_OBJECTS: &[&str] = &[
    "Array",
    "ArrayBuffer",
    "Atomics",
    "BigInt",
    "Boolean",
    "DataView",
    "Date",
    "Error",
    "EvalError",
    "Float32Array",
    "Float64Array",
    "Function",
    "Int8Array",
    "Int16Array",
    "Int32Array",
    "Intl",
    "JSON",
    "Map",
    "Math",
    "Number",
    "Object",
    "Promise",
    "Proxy",
    "RangeError",
    "ReferenceError",
    "Reflect",
    "RegExp",
    "Set",
    "String",
    "Symbol",
    "SyntaxError",
    "TypeError",
    "Uint8Array",
    "Uint16Array",
    "Uint32Array",
    "WeakMap",
    "WeakRef",
    "WeakSet",
    "globalThis",
];

/// Global functions of the language.
const GLOBAL_FUNCTIONS: &[&str] = &[
    "parseInt",
    "parseFloat",
    "isNaN",
    "isFinite",
    "encodeURI",
    "encodeURIComponent",
    "decodeURI",
    "decodeURIComponent",
];

/// Web API globals and their page under `Web/API`.
const WEB_GLOBALS: &[(&str, &str)] = &[
    ("fetch", "Window/fetch"),
    ("setTimeout", "Window/setTimeout"),
    ("setInterval", "Window/setInterval"),
    ("clearTimeout", "Window/clearTimeout"),
    ("clearInterval", "Window/clearInterval"),
    ("requestAnimationFrame", "Window/requestAnimationFrame"),
    ("structuredClone", "Window/structuredClone"),
    ("queueMicrotask", "Window/queueMicrotask"),
    ("alert", "Window/alert"),
    ("confirm", "Window/confirm"),
    ("console", "console"),
    ("document", "Document"),
    ("window", "Window"),
    ("localStorage", "Window/localStorage"),
    ("sessionStorage", "Window/sessionStorage"),
    ("navigator", "Navigator"),
    ("location", "Location"),
    ("history", "History"),
    ("performance", "Performance"),
    ("crypto", "Crypto"),
    ("URL", "URL"),
    ("URLSearchParams", "URLSearchParams"),
    ("AbortController", "AbortController"),
    ("Response", "Response"),
    ("Request", "Request"),
    ("Headers", "Headers"),
    ("FormData", "FormData"),
    ("Blob", "Blob"),
    ("File", "File"),
    ("FileReader", "FileReader"),
    ("Event", "Event"),
    ("CustomEvent", "CustomEvent"),
    ("EventTarget", "EventTarget"),
    ("WebSocket", "WebSocket"),
    ("Worker", "Worker"),
    ("TextEncoder", "TextEncoder"),
    ("TextDecoder", "TextDecoder"),
    ("IntersectionObserver", "IntersectionObserver"),
    ("ResizeObserver", "ResizeObserver"),
    ("MutationObserver", "MutationObserver"),
];

/// Web API objects whose members live on an interface page: `document.querySelector` →
/// `Document/querySelector`. (`console` members have a `_static` suffix.)
const WEB_OBJECTS: &[(&str, &str)] = &[
    ("document", "Document"),
    ("window", "Window"),
    ("localStorage", "Storage"),
    ("sessionStorage", "Storage"),
    ("navigator", "Navigator"),
    ("location", "Location"),
    ("history", "History"),
    ("performance", "Performance"),
    ("crypto", "Crypto"),
];

/// Methods found on one built-in only, called on a value of unknown type (`arr.map(…)`):
/// the built-in, and whether it is a Web API interface.
const UNIQUE_MEMBERS: &[(&str, &str, bool)] = &[
    ("map", "Array", false),
    ("filter", "Array", false),
    ("reduce", "Array", false),
    ("reduceRight", "Array", false),
    ("forEach", "Array", false),
    ("find", "Array", false),
    ("findIndex", "Array", false),
    ("findLast", "Array", false),
    ("findLastIndex", "Array", false),
    ("some", "Array", false),
    ("every", "Array", false),
    ("flat", "Array", false),
    ("flatMap", "Array", false),
    ("push", "Array", false),
    ("pop", "Array", false),
    ("shift", "Array", false),
    ("unshift", "Array", false),
    ("splice", "Array", false),
    ("sort", "Array", false),
    ("reverse", "Array", false),
    ("fill", "Array", false),
    ("join", "Array", false),
    ("toSorted", "Array", false),
    ("toReversed", "Array", false),
    ("toSpliced", "Array", false),
    ("split", "String", false),
    ("trim", "String", false),
    ("trimStart", "String", false),
    ("trimEnd", "String", false),
    ("padStart", "String", false),
    ("padEnd", "String", false),
    ("toUpperCase", "String", false),
    ("toLowerCase", "String", false),
    ("startsWith", "String", false),
    ("endsWith", "String", false),
    ("replace", "String", false),
    ("replaceAll", "String", false),
    ("charAt", "String", false),
    ("charCodeAt", "String", false),
    ("codePointAt", "String", false),
    ("localeCompare", "String", false),
    ("normalize", "String", false),
    ("repeat", "String", false),
    ("substring", "String", false),
    ("match", "String", false),
    ("matchAll", "String", false),
    ("then", "Promise", false),
    ("catch", "Promise", false),
    ("finally", "Promise", false),
    ("toFixed", "Number", false),
    ("toPrecision", "Number", false),
    ("addEventListener", "EventTarget", true),
    ("removeEventListener", "EventTarget", true),
    ("dispatchEvent", "EventTarget", true),
    ("preventDefault", "Event", true),
    ("stopPropagation", "Event", true),
    ("querySelector", "Element", true),
    ("querySelectorAll", "Element", true),
    ("getAttribute", "Element", true),
    ("setAttribute", "Element", true),
    ("closest", "Element", true),
];

/// Built-in member names shared by several built-ins: a search page.
const SHARED_MEMBERS: &[&str] = &[
    "includes",
    "indexOf",
    "lastIndexOf",
    "slice",
    "at",
    "concat",
    "entries",
    "keys",
    "values",
    "has",
    "get",
    "set",
    "add",
    "delete",
    "clear",
    "toString",
    "valueOf",
    "toLocaleString",
    "json",
    "text",
    "abort",
];

fn page(base: &str, path: &str, title: String) -> Option<WebDoc> {
    Some(WebDoc {
        title,
        url: format!("{base}/{path}"),
        exact: true,
    })
}

/// The MDN page for `name` (with `recv`, the name before the dot, if any).
pub fn lookup(name: &str, recv: Option<&str>) -> Option<WebDoc> {
    match recv {
        None => {
            if GLOBAL_OBJECTS.contains(&name) || GLOBAL_FUNCTIONS.contains(&name) {
                return page(JS, name, name.to_string());
            }
            WEB_GLOBALS
                .iter()
                .find(|(n, _)| *n == name)
                .and_then(|(_, p)| page(API, p, name.to_string()))
        }
        Some(obj) => {
            if GLOBAL_OBJECTS.contains(&obj) {
                return page(JS, &format!("{obj}/{name}"), format!("{obj}.{name}"));
            }
            if obj == "console" {
                return page(
                    API,
                    &format!("console/{name}_static"),
                    format!("console.{name}"),
                );
            }
            if let Some((_, iface)) = WEB_OBJECTS.iter().find(|(o, _)| *o == obj) {
                return page(API, &format!("{iface}/{name}"), format!("{iface}.{name}"));
            }
            if let Some((_, owner, web)) = UNIQUE_MEMBERS.iter().find(|(m, _, _)| *m == name) {
                let (base, title) = if *web {
                    (API, format!("{owner}.{name}"))
                } else {
                    (JS, format!("{owner}.prototype.{name}"))
                };
                return page(base, &format!("{owner}/{name}"), title);
            }
            SHARED_MEMBERS.contains(&name).then(|| WebDoc {
                title: format!("{name} (several built-ins)"),
                url: format!("https://developer.mozilla.org/en-US/search?q={name}"),
                exact: false,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_ins_and_web_apis() {
        let url = |n: &str, r: Option<&str>| lookup(n, r).map(|d| d.url);
        assert_eq!(
            url("parse", Some("JSON")).unwrap(),
            format!("{JS}/JSON/parse")
        );
        assert_eq!(url("map", Some("rows")).unwrap(), format!("{JS}/Array/map"));
        assert_eq!(
            lookup("map", Some("rows")).unwrap().title,
            "Array.prototype.map"
        );
        assert_eq!(
            url("push", Some("out")).unwrap(),
            format!("{JS}/Array/push")
        );
        assert_eq!(url("fetch", None).unwrap(), format!("{API}/Window/fetch"));
        assert_eq!(url("Promise", None).unwrap(), format!("{JS}/Promise"));
        assert_eq!(
            url("log", Some("console")).unwrap(),
            format!("{API}/console/log_static")
        );
        assert_eq!(
            url("getItem", Some("localStorage")).unwrap(),
            format!("{API}/Storage/getItem")
        );
        let inc = lookup("includes", Some("x")).unwrap();
        assert!(!inc.exact && inc.url.ends_with("search?q=includes"));
        // The project's own names get nothing.
        assert!(lookup("formatCurrency", None).is_none());
        assert!(lookup("save", Some("store")).is_none());
    }
}
