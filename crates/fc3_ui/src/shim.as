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
// arguments; "Get" and "GetPlatform" put its answer where they say.
_global.__fireEngineCall = function(command) {
    var args = new Array();
    var i = 1;
    while (i < arguments.length) {
        args.push(arguments[i]);
        i++;
    }
    var result = flash.external.ExternalInterface.call("engineCall", command, args);
    if (command == "Get") {
        _global.__fireSetPath(args[1], result);
    } else if (command == "GetPlatform") {
        _global.__fireSetPath(args[0], result);
    }
    return result;
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
// loaded movies.
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
_global.__fireAt = function(path) {
    var parts = String(path).split(".");
    var o = _global;
    var i = 0;
    if (parts[0] == "_global") {
        i = 1;
    } else if (parts[0].substr(0, 6) == "_level" || parts[0] == "_root") {
        o = eval(parts[0]);
        i = 1;
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
