// The engine's side of the game's interface movies (Dunia's "Fire"
// framework), for Ruffle: the event channels the movies listen on, the call
// they make on the engine (their FSCommand2, made a call of
// __fireEngineCall by fc3_formats::avm1), and what the engine does to their
// clips by the DPS ids they give them. The host (fc3_ui::Host) talks to it
// through ExternalInterface.

// Event channels: _global.ExtendedEvents.<name> is a broadcaster the movies
// add listeners to ({onEvent: f}); the host broadcasts onEvent(args...).
_global.ExtendedEvents = new Object();
_global.ExtendedEvents.__resolve = function(name) {
    var channel = new Object();
    AsBroadcaster.initialize(channel);
    this[name] = channel;
    return channel;
};

// A path as the engine's Get takes it ("_global.x", "_root.a.b", "x").
_global.__fireSetPath = function(path, value) {
    if (path == undefined) {
        return;
    }
    var parts = String(path).split(".");
    var o = _global;
    var i = 0;
    if (parts[0] == "_global") {
        i = 1;
    } else if (parts[0] == "_root") {
        o = _root;
        i = 1;
    }
    while (i < parts.length - 1) {
        if (o[parts[i]] == undefined) {
            o[parts[i]] = new Object();
        }
        o = o[parts[i]];
        i++;
    }
    o[parts[parts.length - 1]] = value;
};

// A movie's call on the engine: the host gets the command and its
// arguments; "Get" and "GetPlatform" put its answer where they say. A list
// the host has ready for a page's request (`prepareData`) goes to the page
// at once, as the engine answered (the shop page reads what it showed as it
// hears of the answer, before it goes on); the host still hears of the
// request, marked "prepared".
_global.__firePrepared = new Object();
_global.__fireEngineCall = function(command) {
    var args = new Array();
    var i = 1;
    while (i < arguments.length) {
        args.push(arguments[i]);
        i++;
    }
    var prepared = undefined;
    if (command == "Send" && args[0] == "onRequestedMenu") {
        prepared = _global.__firePrepared[String(args[2]) + ":" + String(args[1])];
        if (prepared != undefined) {
            args.push("prepared");
        }
    }
    var result = flash.external.ExternalInterface.call("engineCall", command, args);
    if (command == "Get") {
        _global.__fireSetPath(args[1], result);
    } else if (command == "GetPlatform") {
        _global.__fireSetPath(args[0], result);
    }
    if (prepared != undefined) {
        _global.__fireDeliver(String(args[2]), prepared);
    }
    return result;
};

// The movies' sounds: each exports its sounds by name with nothing behind
// them (`Play_SFX_HUD_New_SubObjective`); the engine played the sound event
// of that name's id (the movie's own table, `fc3_formats::feu::sound_ids`).
// `attachSound` keeps the name, and `start` hands it to the host
// (`engineSound`) instead of playing nothing. (Both are read-only as the
// player declares them: the flag cleared first.)
ASSetPropFlags(Sound.prototype, ["attachSound", "start"], 0, 4);
Sound.prototype.__fireAttach = Sound.prototype.attachSound;
Sound.prototype.attachSound = function(name) {
    this.__fireName = name;
    this.__fireAttach(name);
};
Sound.prototype.__fireStart = Sound.prototype.start;
Sound.prototype.start = function() {
    if (this.__fireName != undefined) {
        flash.external.ExternalInterface.call("engineSound", String(this.__fireName));
    }
};

// The engine's string table: fire.utils.Localizer.getText(id) gives the
// text of a string id (the movies have no Localizer of their own).
if (_global.fire == undefined) {
    _global.fire = new Object();
}
if (_global.fire.utils == undefined) {
    _global.fire.utils = new Object();
}
_global.fire.utils.Localizer = new Object();
_global.fire.utils.Localizer.getText = function(id) {
    var t = flash.external.ExternalInterface.call("engineText", String(id));
    return t == null ? "" : t;
};

// The engine's distance-field text (its fonts drawn from distance fields,
// outlines and glows on them): the movies' fire.utils.DistanceFieldManager
// wraps an engine object, `new fire.utils.DistanceFieldManagerPrivate()`,
// which they don't define, and constructing nothing failed as the HUD
// loaded. Our text is drawn from traced outlines, so it takes what it is
// given and does nothing with it.
_global.fire.utils.DistanceFieldManagerPrivate = function() {
};
_global.fire.utils.DistanceFieldManagerPrivate.prototype.setCallback = function() {
};
_global.fire.utils.DistanceFieldManagerPrivate.prototype.deleteCallback = function() {
};

// The engine's tweens, fire.utils.TweenEx (no movie defines it; the menus
// fade and slide everything with it): new TweenEx(clip, seconds, {prop: to
// or [from, to], ease: fire.utils.EaseType.*, delay, repeat (-1 for ever),
// yoyo, onComplete, onCompleteParams}), stepped by the clock every frame.
_global.__fireTweens = new Array();
_global.fire.utils.TweenEx = function(target, duration, props) {
    this.target = target;
    this.duration = Number(duration);
    this.ease = 0;
    this.delay = 0;
    this.repeat = 0;
    this.yoyo = false;
    this.props = new Array();
    for (var k in props) {
        var v = props[k];
        if (k == "ease") {
            this.ease = Number(v);
        } else if (k == "delay") {
            this.delay = Number(v);
        } else if (k == "repeat") {
            this.repeat = Number(v);
        } else if (k == "yoyo") {
            this.yoyo = Boolean(v);
        } else if (k == "onComplete") {
            this.onComplete = v;
        } else if (k == "onCompleteParams") {
            this.onCompleteParams = v;
        } else if (k == "onCompleteScope") {
            this.onCompleteScope = v;
        } else {
            var p = new Object();
            p.name = k;
            if (v instanceof Array) {
                p.from = Number(v[0]);
                p.to = Number(v[1]);
                target[k] = p.from;
            } else {
                p.to = Number(v);
            }
            this.props.push(p);
        }
    }
    this.start = getTimer() + this.delay * 1000;
    this.started = false;
    this.backwards = false;
    this.stopped = false;
    _global.__fireTweens.push(this);
};
_global.fire.utils.TweenEx.prototype.stop = function() {
    this.stopped = true;
};
// The engine's tween of one property, fire.utils.Tween (the frontend's):
// new Tween(clip, property, ease name, from, to, seconds, delay, -, loop,
// event). An alpha given as a fraction (both ends at most 1) goes as a
// percentage; a loop goes back and forth for ever; at its end the event,
// if one is named, is heard on its channel (`onBulletinShowEnd`).
_global.__fireEaseNames = {EaseNone: 0, EaseRegularIn: 1, EaseRegularOut: 2, EaseRegularInOut: 3, EaseStrongIn: 4, EaseStrongOut: 5, EaseStrongInOut: 6, EaseBackIn: 7, EaseBackOut: 8, EaseBackInOut: 9, EaseBounceIn: 10, EaseBounceOut: 11, EaseBounceInOut: 12};
_global.fire.utils.Tween = function(target, prop, ease, from, to, duration, delay, unused, loop, eventName) {
    var a = Number(from);
    var b = Number(to);
    if (prop == "_alpha" && Math.abs(a) <= 1 && Math.abs(b) <= 1) {
        a = a * 100;
        b = b * 100;
    }
    var props = new Object();
    props[prop] = [a, b];
    var e = _global.__fireEaseNames[String(ease)];
    props.ease = e == undefined ? 0 : e;
    if (Number(delay) > 0) {
        props.delay = Number(delay);
    }
    if (loop == true) {
        props.repeat = -1;
        props.yoyo = true;
    }
    if (eventName != undefined && eventName != null && String(eventName) != "") {
        var channel = String(eventName);
        props.onComplete = function() {
            _global.ExtendedEvents[channel].broadcastMessage("onEvent");
        };
    }
    this.tween = new _global.fire.utils.TweenEx(target, duration, props);
};
_global.fire.utils.Tween.prototype.stop = function() {
    this.tween.stop();
};
// fire.utils.EaseType: None, Regular (quadratic), Strong (quintic), Back,
// Bounce, each In/Out/InOut from 1; Bezier (13) goes as None.
_global.__fireBounce = function(t) {
    if (t < 1 / 2.75) {
        return 7.5625 * t * t;
    } else if (t < 2 / 2.75) {
        t -= 1.5 / 2.75;
        return 7.5625 * t * t + 0.75;
    } else if (t < 2.5 / 2.75) {
        t -= 2.25 / 2.75;
        return 7.5625 * t * t + 0.9375;
    }
    t -= 2.625 / 2.75;
    return 7.5625 * t * t + 0.984375;
};
_global.__fireEaseIn = function(kind, t) {
    if (kind == 0) {
        return t * t;
    } else if (kind == 1) {
        return t * t * t * t * t;
    } else if (kind == 2) {
        return t * t * (2.70158 * t - 1.70158);
    }
    return 1 - _global.__fireBounce(1 - t);
};
_global.__fireEase = function(ease, t) {
    if (ease <= 0 || ease > 12) {
        return t;
    }
    var kind = Math.floor((ease - 1) / 3);
    var way = (ease - 1) % 3;
    if (way == 0) {
        return _global.__fireEaseIn(kind, t);
    } else if (way == 1) {
        return 1 - _global.__fireEaseIn(kind, 1 - t);
    } else if (t < 0.5) {
        return _global.__fireEaseIn(kind, t * 2) / 2;
    }
    return 1 - _global.__fireEaseIn(kind, (1 - t) * 2) / 2;
};
_global.__fireTweenStep = function() {
    var now = getTimer();
    var list = _global.__fireTweens;
    var i = 0;
    while (i < list.length) {
        var t = list[i];
        if (t.stopped || t.target == undefined) {
            list.splice(i, 1);
        } else if (now < t.start) {
            i++;
        } else {
            var j = 0;
            if (!t.started) {
                t.started = true;
                while (j < t.props.length) {
                    if (t.props[j].from == undefined) {
                        t.props[j].from = Number(t.target[t.props[j].name]);
                    }
                    j++;
                }
            }
            var f = t.duration > 0 ? (now - t.start) / (t.duration * 1000) : 1;
            var done = f >= 1;
            if (done) {
                f = 1;
            }
            var e = _global.__fireEase(t.ease, t.backwards ? 1 - f : f);
            j = 0;
            while (j < t.props.length) {
                var p = t.props[j];
                t.target[p.name] = p.from + (p.to - p.from) * e;
                j++;
            }
            if (done && t.repeat != 0) {
                if (t.repeat > 0) {
                    t.repeat = t.repeat - 1;
                }
                if (t.yoyo) {
                    t.backwards = !t.backwards;
                }
                t.start = now;
                i++;
            } else if (done) {
                list.splice(i, 1);
                if (t.onComplete != undefined) {
                    t.onComplete.apply(t.onCompleteScope == undefined ? t.target : t.onCompleteScope, t.onCompleteParams == undefined ? new Array() : t.onCompleteParams);
                }
            } else {
                i++;
            }
        }
    }
};
this.onEnterFrame = function() {
    _global.__fireTweenStep();
};

// The clips the engine drives, by DPS id (_fireDPSId), found by walking the
// loaded movies. (The knife thrower's movie spells a few _fireDPSID, which
// the engine took as well.)
_global.__fireDps = new Object();
_global.__fireScan = function(clip, depth) {
    if (depth > 32) {
        return;
    }
    for (var k in clip) {
        var c = clip[k];
        if ((typeof(c) == "movieclip" || c instanceof TextField) && c._parent == clip) {
            if (c._fireDPSId != undefined) {
                _global.__fireDps[c._fireDPSId] = c;
            } else if (c._fireDPSID != undefined) {
                _global.__fireDps[c._fireDPSID] = c;
            }
            if (typeof(c) == "movieclip") {
                _global.__fireScan(c, depth + 1);
            }
        }
    }
};
_global.__fireRescan = function() {
    _global.__fireDps = new Object();
    var level = 0;
    while (level < 64) {
        var root = eval("_level" + level);
        if (root != undefined) {
            if (root._fireDPSId != undefined) {
                _global.__fireDps[root._fireDPSId] = root;
            }
            _global.__fireScan(root, 0);
        }
        level++;
    }
};
_global.__fireFind = function(id) {
    var c = _global.__fireDps[id];
    if (c == undefined || (c._parent == undefined && c._name != "")) {
        _global.__fireRescan();
        c = _global.__fireDps[id];
    }
    return c;
};

var api = flash.external.ExternalInterface;
api.addCallback("load", null, function(url, level) {
    loadMovieNum(url, level);
});
api.addCallback("unload", null, function(level) {
    unloadMovieNum(level);
});
api.addCallback("levelVisible", null, function(level, on) {
    var root = eval("_level" + level);
    if (root != undefined) {
        root._visible = on;
    }
});
api.addCallback("event", null, function(name, args) {
    var channel = _global.ExtendedEvents[name];
    // (A blank string comes from the host as null.)
    for (var k = 0; k < args.length; k++) {
        if (args[k] === null) {
            args[k] = "";
        }
    }
    channel.broadcastMessage.apply(channel, ["onEvent"].concat(args));
});
// A property at a path set (`_level20.bg.black._visible`: a clip the
// engine drew over); false if the path isn't there.
api.addCallback("setPath", null, function(path, value) {
    var parts = String(path).split(".");
    var o = _global;
    var i = 0;
    if (parts[0] == "_global") {
        i = 1;
    } else if (parts[0].substr(0, 6) == "_level" || parts[0] == "_root") {
        o = eval(parts[0]);
        i = 1;
    }
    while (i < parts.length - 1) {
        o = o[parts[i]];
        if (o == undefined) {
            return false;
        }
        i++;
    }
    o[parts[parts.length - 1]] = value;
    return true;
});
// A function a movie defines (on its timeline or globally), by path.
api.addCallback("call", null, function(path, args) {
    var parts = String(path).split(".");
    var o = _global;
    var i = 0;
    if (parts[0] == "_global") {
        i = 1;
    } else if (parts[0].substr(0, 6) == "_level" || parts[0] == "_root") {
        o = eval(parts[0]);
        i = 1;
    }
    while (i < parts.length - 1) {
        o = o[parts[i]];
        i++;
    }
    var f = o[parts[parts.length - 1]];
    // (A blank string comes from the host as null.)
    for (var k = 0; k < args.length; k++) {
        if (args[k] === null) {
            args[k] = "";
        }
    }
    return f.apply(o, args);
});
// The engine's data providers, fire.utils.DataProvider(name) (no movie
// defines it; lib's DataProviderAS is the script-side kind): the lists a
// page shows (the rucksack's loot, recipes, skills...), kept here by name as
// the host sets them. A page reads them (getLength, request) and hears of
// each new list (onDataReceived), which the host sends once the page says
// it is loaded (a page builds its grid on it); an item's fields bind to the
// clips of the same names on its tile (fire.utils.DataBinderUtils).
_global.__fireData = new Object();
_global.__fireProviders = new Object();
_global.__fireResult = function(status, data) {
    var r = _global.fire.utils.DataResult == undefined ? new Object() : new _global.fire.utils.DataResult();
    r.status = status;
    r.data = data;
    return r;
};
_global.fire.utils.DataProvider = function(name) {
    this.m_name = String(name);
    if (_global.__fireProviders[this.m_name] == undefined) {
        _global.__fireProviders[this.m_name] = new Array();
    }
    _global.__fireProviders[this.m_name].push(this);
};
_global.fire.utils.DataProvider.prototype.getLength = function() {
    var d = _global.__fireData[this.m_name];
    return d == undefined ? 0 : d.length;
};
_global.fire.utils.DataProvider.prototype.request = function(index, count) {
    var d = _global.__fireData[this.m_name];
    var out = new Array();
    if (d == undefined || index < 0 || index >= d.length) {
        return _global.__fireResult(0, out);
    }
    var n = (count == undefined || count < 1) ? 1 : count;
    var i = 0;
    while (i < n && index + i < d.length) {
        out.push(d[index + i]);
        i++;
    }
    return _global.__fireResult(2, out);
};
_global.fire.utils.DataProvider.prototype.onDataReceived = function(result) {
    return true;
};
// A list's items as the host sends them, made ready for the page. (A blank
// string comes from the host as null; bound to a text field it would read
// "null".) A button's state binds after its other fields, as the engine's
// lists had it (a shop button's state reads its price): fields go in
// reverse of their making, so it's made first, the others after in their
// order.
_global.__fireRows = function(items) {
    for (var n = 0; n < items.length; n++) {
        var src = items[n];
        var names = new Array();
        for (var f in src) {
            names.push(f);
        }
        var item = new Object();
        if (src.iStateID !== undefined) {
            item.iStateID = src.iStateID === null ? "" : src.iStateID;
        }
        for (var k = names.length - 1; k >= 0; k--) {
            if (names[k] != "iStateID") {
                item[names[k]] = src[names[k]] === null ? "" : src[names[k]];
            }
        }
        items[n] = item;
    }
    return items;
};
// A list in place of the last of its name, the pages showing it told.
_global.__fireDeliver = function(name, items) {
    _global.__fireData[name] = items;
    var list = _global.__fireProviders[name];
    var i = 0;
    while (list != undefined && i < list.length) {
        list[i].onDataReceived(_global.__fireResult(2, items));
        i++;
    }
};
api.addCallback("setData", null, function(name, items) {
    _global.__fireDeliver(name, _global.__fireRows(items));
});
// A list ready for a request ("<list>:<id>"); a blank key forgets them all.
api.addCallback("prepareData", null, function(key, items) {
    if (key == null || key == "") {
        _global.__firePrepared = new Object();
    } else {
        _global.__firePrepared[key] = _global.__fireRows(items);
    }
});
api.addCallback("dpsList", null, function() {
    _global.__fireRescan();
    var ids = new Array();
    for (var id in _global.__fireDps) {
        ids.push(id);
    }
    return ids;
});
// A clip's state at a glance (tools): kind, frames, visibility, place,
// size and bounds on the stage, a text field's text.
api.addCallback("dpsInfo", null, function(id) {
    var c = _global.__fireFind(id);
    if (c == undefined) {
        return undefined;
    }
    var b = c.getBounds == undefined ? undefined : c.getBounds(_root);
    var s = (c instanceof TextField) ? "text \"" + c.text + "\"" : typeof(c) + " frame " + c._currentframe + "/" + c._totalframes;
    s += " visible " + c._visible + " at " + Math.round(c._x) + "," + Math.round(c._y) + " size " + Math.round(c._width) + "x" + Math.round(c._height);
    if (b != undefined) {
        s += " stage " + Math.round(b.xMin) + "," + Math.round(b.yMin) + " to " + Math.round(b.xMax) + "," + Math.round(b.yMax);
    }
    return s;
});
// (Tools) what is at a path ("_level1.mcBack", "_global.isInverted"): a
// clip's state at a glance, else the value.
// (A bare _global read as a value comes out undefined here: start from its
// first member.)
_global.__fireAt = function(path) {
    var parts = String(path).split(".");
    var o;
    var i = 1;
    if (parts[0] == "_global") {
        o = _global[parts[1]];
        i = 2;
    } else if (parts[0].substr(0, 6) == "_level" || parts[0] == "_root") {
        o = eval(parts[0]);
    } else {
        o = _global[parts[0]];
    }
    while (i < parts.length && o != undefined) {
        o = o[parts[i]];
        i++;
    }
    return o;
};
api.addCallback("getPath", null, function(path) {
    var o = _global.__fireAt(path);
    if (typeof(o) == "movieclip") {
        return String(o) + " frame " + o._currentframe + "/" + o._totalframes + " visible " + o._visible + " alpha " + o._alpha + " at " + Math.round(o._x) + "," + Math.round(o._y) + " size " + Math.round(o._width) + "x" + Math.round(o._height);
    }
    return o == undefined ? "undefined" : String(o);
});
// (Tools) an object's members and their kinds.
api.addCallback("listPath", null, function(path) {
    var o = _global.__fireAt(path);
    var out = new Array();
    for (var k in o) {
        out.push(k + ":" + typeof(o[k]));
    }
    return out;
});
// A clip's bounds on the stage: [xMin, yMin, xMax, yMax].
api.addCallback("dpsBounds", null, function(id) {
    var c = _global.__fireFind(id);
    if (c == undefined || c.getBounds == undefined) {
        return undefined;
    }
    var b = c.getBounds(_root);
    return [b.xMin, b.yMin, b.xMax, b.yMax];
});
// Whether a clip's page shows it: its parents all shown, up to its level
// (its own visibility is the engine's, which hides the stand-ins it draws
// over).
api.addCallback("dpsShown", null, function(id) {
    var c = _global.__fireFind(id);
    if (c == undefined || c._parent == undefined) {
        return false;
    }
    var p = c._parent;
    while (p != undefined) {
        if (p._visible != true || !(p._alpha > 0)) {
            return false;
        }
        p = p._parent;
    }
    return true;
});
// (An empty text comes across as null.)
api.addCallback("dpsText", null, function(id, text) {
    var c = _global.__fireFind(id);
    if (c == undefined) {
        return false;
    }
    c.text = (text == null) ? "" : text;
    return true;
});
api.addCallback("dpsHtml", null, function(id, html) {
    var c = _global.__fireFind(id);
    if (c == undefined) {
        return false;
    }
    c.htmlText = (html == null) ? "" : html;
    return true;
});
api.addCallback("dpsPlay", null, function(id, frame) {
    var c = _global.__fireFind(id);
    if (c == undefined) {
        return false;
    }
    c.gotoAndPlay(frame);
    return true;
});
api.addCallback("dpsVisible", null, function(id, on) {
    var c = _global.__fireFind(id);
    if (c == undefined) {
        return false;
    }
    c._visible = on;
    return true;
});
api.addCallback("dpsFrame", null, function(id, frame) {
    var c = _global.__fireFind(id);
    if (c == undefined) {
        return false;
    }
    c.gotoAndStop(frame);
    return true;
});
api.addCallback("dpsSet", null, function(id, property, value) {
    var c = _global.__fireFind(id);
    if (c == undefined) {
        return false;
    }
    c[property] = value;
    return true;
});
api.addCallback("dpsSetParent", null, function(id, property, value) {
    var c = _global.__fireFind(id);
    if (c == undefined || c._parent == undefined) {
        return false;
    }
    c._parent[property] = value;
    return true;
});
api.addCallback("dpsGet", null, function(id, property) {
    var c = _global.__fireFind(id);
    if (c == undefined) {
        return undefined;
    }
    return c[property];
});
