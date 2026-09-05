package vixen

// domtest: DOM bindings against corpus/domtest.html.
//   vixen domtest [page.html]

import "core:fmt"
import "core:os"

domtest_main :: proc(path: string) -> bool {
	data, err := os.read_entire_file_from_path(path, context.allocator)
	if err != nil {
		fmt.eprintfln("domtest: cannot read %s", path)
		return false
	}
	defer delete(data)
	dc: Dom_Ctx
	if !dom_context_new(data, &dc) {
		fmt.eprintfln("domtest: context failed")
		return false
	}
	defer dom_context_free(&dc)
	fails := 0
	check :: proc(dc: ^Dom_Ctx, fails: ^int, name, src, want: string) {
		got, ok := dom_eval(dc, src)
		defer delete(got)
		pass := ok && got == want
		if !pass {
			fails^ += 1
		}
		fmt.printfln("%s %-24s got=%q want=%q", "PASS" if pass else "FAIL", name, got, want)
	}
	check(&dc, &fails, "query/title", `document.querySelector('title').textContent`, "DOM Test")
	check(&dc, &fails, "byid/tag", `document.getElementById('main').tagName`, "DIV")
	check(&dc, &fails, "qsa/length", `document.querySelectorAll('p.item').length`, "2")
	check(&dc, &fails, "query/descendant", `document.querySelector('#main .item').textContent`, "first")
	check(&dc, &fails, "attr/get", `document.getElementById('link').getAttribute('href')`, "/next")
	check(&dc, &fails, "attr/missing-null", `String(document.getElementById('link').getAttribute('nope'))`, "null")
	check(&dc, &fails, "attr/set", `document.getElementById('link').setAttribute('href','/other'); document.getElementById('link').getAttribute('href')`, "/other")
	check(&dc, &fails, "text/set", `document.getElementById('t').textContent = 'hi'; document.getElementById('t').textContent`, "hi")
	check(&dc, &fails, "body/tag", `document.body.tagName`, "BODY")
	check(&dc, &fails, "docel/tag", `document.documentElement.tagName`, "HTML")
	check(&dc, &fails, "nodetype", `document.getElementById('main').nodeType`, "1")
	check(&dc, &fails, "parent", `document.getElementById('link').parentNode.id`, "main")
	check(&dc, &fails, "children", `document.getElementById('main').childNodes.length`, "11")
	check(&dc, &fails, "inner", `document.getElementById('link').outerHTML`, `<a id="link" href="/other">go</a>`)
	check(&dc, &fails, "create/append", `const d = document.createElement('div'); d.textContent='new'; document.getElementById('main').appendChild(d); document.querySelectorAll('div').length`, "2")
	check(&dc, &fails, "remove", `const p = document.querySelector('p.item'); document.getElementById('main').removeChild(p); document.querySelectorAll('p.item').length`, "1")
	// Events: capture -> target -> bubble order, then stopPropagation.
	check(&dc, &fails, "events/order", `var log = [];
document.getElementById('main').addEventListener('click', () => log.push('bubble-main'));
document.getElementById('main').addEventListener('click', () => log.push('capture-main'), true);
document.getElementById('btn').addEventListener('click', () => log.push('target'));
var ev = document.createEvent('click');
document.getElementById('btn').dispatchEvent(ev);
log.join(',')`, "capture-main,target,bubble-main")
	check(&dc, &fails, "events/stop", `var log2 = [];
document.getElementById('main').addEventListener('stop', () => log2.push('parent'));
document.getElementById('btn').addEventListener('stop', (e) => { log2.push('child'); e.stopPropagation(); });
var ev2 = document.createEvent('stop');
document.getElementById('btn').dispatchEvent(ev2);
log2.join(',')`, "child")
	check(&dc, &fails, "events/remove", `var n = 0;
const h = () => n++;
document.getElementById('btn').addEventListener('gone', h);
document.getElementById('btn').removeEventListener('gone', h);
document.getElementById('btn').dispatchEvent(document.createEvent('gone'));
String(n)`, "0")
	check(&dc, &fails, "events/target", `var seen = '';
document.getElementById('main').addEventListener('t2', (e) => { seen = e.target.id + '/' + e.type; });
document.getElementById('btn').dispatchEvent(document.createEvent('t2'));
seen`, "btn/t2")
	fmt.printfln("domtest: %d failures", fails)
	return fails == 0
}
