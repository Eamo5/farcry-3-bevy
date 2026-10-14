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
