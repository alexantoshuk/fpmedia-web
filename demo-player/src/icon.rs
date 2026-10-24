//! Demo chrome icons. Unused glyphs are kept for upcoming UI — leptos props on
//! unused components trip `dead_code` under `-D warnings`.
#![allow(dead_code)]

use leptos::prelude::*;

#[component]
pub fn Play(#[prop(into, optional)] class: Signal<String>) -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" fill="currentColor" class=class>
            <path d="m6.424 4.097 12.031 7.158a.86.86 0 0 1 0 1.477l-12.03 7.17a.86.86 0 0 1-1.3-.739V4.836a.86.86 0 0 1 1.299-.739" />
        </svg>
    }
}

#[component]
pub fn Pause(#[prop(into, optional)] class: Signal<String>) -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" fill="currentColor" class=class>
            <path d="M6.437 3.98h1.96c.455 0 .62.053.788.153a1 1 0 0 1 .387.435c.09.188.136.374.136.887v13.09c0 .513-.047.699-.137.887q-.134.283-.386.435c-.252.153-.333.154-.789.154H6.437c-.456 0-.621-.054-.788-.154a1 1 0 0 1-.388-.435c-.089-.188-.136-.374-.136-.887V5.455c0-.513.047-.699.138-.887a1 1 0 0 1 .386-.435c.167-.1.332-.154.788-.154m9.167 0h1.959c.456 0 .621.054.788.154a1 1 0 0 1 .388.435c.089.188.136.374.136.887v13.09c0 .513-.047.699-.137.887a1 1 0 0 1-.387.435c-.167.1-.332.154-.788.154h-1.96c-.455 0-.62-.054-.788-.154a1 1 0 0 1-.387-.435c-.09-.188-.136-.374-.136-.887V5.455c0-.513.047-.699.137-.887a1 1 0 0 1 .386-.435c.168-.1.333-.154.789-.154" />
        </svg>
    }
}

#[component]
pub fn NextFrame(#[prop(into, optional)] class: Signal<String>) -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" fill="currentColor" class=class>
            <path d="m12.31 4.125 7.339 7.132c.335.333.335 1.14 0 1.473l-7.338 7.143c-.343.342-1.357.032-1.357-.633V4.76c0-.664 1.012-.974 1.355-.635m-6.901-.119h.941c.455 0 .62.053.786.153a1 1 0 0 1 .386.434c.09.187.136.372.136.883v13.047c0 .512-.047.697-.137.884a1 1 0 0 1-.385.434c-.251.152-.331.153-.786.153h-.941c-.455 0-.62-.053-.786-.153a1 1 0 0 1-.386-.434c-.09-.187-.136-.372-.136-.884V5.476c0-.511.047-.696.137-.883a1 1 0 0 1 .385-.434c.167-.1.331-.153.786-.153" />
        </svg>
    }
}

#[component]
pub fn Volume0(#[prop(into, optional)] class: Signal<String>) -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" fill="currentColor" class=class>
            <path d="m20 10.763 2.381-2.382A.875.875 0 1 1 23.62 9.62L21.237 12l2.382 2.381a.875.875 0 1 1-1.238 1.238L20 13.237l-2.381 2.382a.875.875 0 1 1-1.238-1.238L18.763 12 16.38 9.619a.875.875 0 1 1 1.24-1.239zm-7.02 8.06c-.031 1.207-.67 1.609-1.828.62-1.719-1.469-3.609-3.194-4.242-3.721-.632-.529-1.645-.62-3.073-.62s-1.815-.621-1.815-1.241-.014-1.83-.014-2.001c0-.055.005-.086.014-.13.02-.095-.058-.973 0-1.59.085-.906.388-1.241 1.815-1.241s2.44-.092 3.073-.62c.633-.529 2.523-2.253 4.242-3.722 1.158-.989 1.797-.587 1.829.62.043 1.607 0 3.85 0 6.683 0 2.83.043 5.356 0 6.963" />
        </svg>
    }
}

#[component]
pub fn Volume1(#[prop(into, optional)] class: Signal<String>) -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" fill="currentColor" class=class>
            <path d="M12.98 18.823c-.031 1.207-.67 1.609-1.828.62-1.719-1.469-3.609-3.194-4.242-3.721-.632-.529-1.645-.62-3.073-.62s-1.815-.621-1.815-1.241-.014-1.83-.014-2.001c0-.055.005-.086.014-.13.02-.095-.058-.973 0-1.59.085-.906.388-1.241 1.815-1.241s2.44-.092 3.073-.62c.633-.529 2.523-2.253 4.242-3.722 1.158-.989 1.797-.587 1.829.62.043 1.607 0 3.85 0 6.683 0 2.83.043 5.356 0 6.963m2.901-8.704A.875.875 0 1 1 17.12 8.88c.176.177.41.47.64.872.384.671.616 1.425.616 2.247s-.232 1.576-.615 2.247a4.2 4.2 0 0 1-.641.872.875.875 0 1 1-1.238-1.238c.074-.073.214-.249.36-.503.241-.423.384-.887.384-1.378s-.143-.955-.385-1.378a2.6 2.6 0 0 0-.359-.503" />
        </svg>
    }
}

#[component]
pub fn Volume2(#[prop(into, optional)] class: Signal<String>) -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" fill="currentColor" class=class>
            <path d="M12.98 18.823c-.031 1.207-.67 1.609-1.828.62-1.719-1.469-3.609-3.194-4.242-3.721-.632-.529-1.645-.62-3.073-.62s-1.815-.621-1.815-1.241-.014-1.83-.014-2.001c0-.055.005-.086.014-.13.02-.095-.058-.973 0-1.59.085-.906.388-1.241 1.815-1.241s2.44-.092 3.073-.62c.633-.529 2.523-2.253 4.242-3.722 1.158-.989 1.797-.587 1.829.62.043 1.607 0 3.85 0 6.683 0 2.83.043 5.356 0 6.963m2.901-8.704A.875.875 0 1 1 17.12 8.88c.176.177.41.47.64.872.384.671.616 1.425.616 2.247s-.232 1.576-.615 2.247a4.2 4.2 0 0 1-.641.872.875.875 0 1 1-1.238-1.238c.074-.073.214-.249.36-.503.241-.423.384-.887.384-1.378s-.143-.955-.385-1.378a2.6 2.6 0 0 0-.359-.503m3-2A.875.875 0 1 1 20.12 6.88c.263.264.62.74.966 1.432.517 1.033.843 2.255.89 3.716-.047 1.403-.373 2.625-.89 3.658-.346.692-.703 1.168-.966 1.432a.875.875 0 1 1-1.238-1.238c.138-.137.383-.464.639-.976.407-.814.667-1.79.705-2.876-.038-1.144-.298-2.12-.705-2.934-.256-.512-.501-.839-.639-.976" />
        </svg>
    }
}

#[component]
pub fn FullScreenEnter(#[prop(into, optional)] class: Signal<String>) -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" fill="currentColor" class=class>
            <path d="M10.5 19a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1v-4.5a1 1 0 0 1 2 0v2.072l3.363-3.365a1 1 0 0 1 1.414 0l.016.016a1 1 0 0 1 0 1.414L7.429 18H9.5a1 1 0 0 1 1 1M18 7.429l-3.363 3.364a1 1 0 0 1-1.414 0l-.016-.016a1 1 0 0 1 0-1.414L16.571 6H14.5a1 1 0 0 1 0-2H19a1 1 0 0 1 1 1v4.5a1 1 0 0 1-2 0z" />
        </svg>
    }
}

#[component]
pub fn FullScreenExit(#[prop(into, optional)] class: Signal<String>) -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" fill="currentColor" class=class>
            <path d="M4.58 13.979a.99.99 0 0 1 .99-.99h4.451a.99.99 0 0 1 .99.99v4.452a.99.99 0 0 1-1.98 0v-2.05l-3.319 3.322a1 1 0 0 1-1.414 0v-.001a1 1 0 0 1 0-1.414l3.32-3.32H5.57a.99.99 0 0 1-.99-.99m10.388-6.36 3.32-3.322a1 1 0 0 1 1.414 0v.001a1 1 0 0 1 0 1.414l-3.32 3.32h2.049a.99.99 0 0 1 0 1.979h-4.442a1 1 0 0 1-1-1V5.569a.99.99 0 1 1 1.98 0z" />
        </svg>
    }
}

#[component]
pub fn Loop(#[prop(into, optional)] class: Signal<String>) -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" fill="currentColor" class=class>
            <path d="M7 7h10v3l4-4-4-4v3H5v6h2V7zm10 10H7v-3l-4 4 4 4v-3h12v-6h-2v4z"></path>
        </svg>
    }
}
#[component]
pub fn Repeat(#[prop(into, optional)] class: Signal<String>) -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" fill="currentColor" class=class>
            <path d="M12 4.5a8 8 0 1 1-8 8 1 1 0 0 1 2 0 6 6 0 1 0 6-6v3.096a.4.4 0 0 1-.67.296L7.044 5.993a.667.667 0 0 1 0-.986l4.288-3.899a.4.4 0 0 1 .669.296z" />
        </svg>
    }
}

#[component]
pub fn CollapseArrow(#[prop(into, optional)] class: Signal<String>) -> impl IntoView {
    view! {
        <svg viewBox="0 0 20 20" class=class>
            <path
                fill-rule="evenodd"
                clip-rule="evenodd"
                d="M5.21967 8.21967C5.51256 7.92678 5.98744 7.92678 6.28033 8.21967L10 11.9393L13.7197 8.21967C14.0126 7.92678 14.4874 7.92678 14.7803 8.21967C15.0732 8.51256 15.0732 8.98744 14.7803 9.28033L10.5303 13.5303C10.3897 13.671 10.1989 13.75 10 13.75C9.80109 13.75 9.61032 13.671 9.46967 13.5303L5.21967 9.28033C4.92678 8.98744 4.92678 8.51256 5.21967 8.21967Z"
                fill="currentColor"
            ></path>
        </svg>
    }
}

#[component]
pub fn Checkmark(#[prop(into, optional)] class: Signal<String>) -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" fill="none" class=class>
            <path
                d="M5.5 11.5 10 16l8.25-8.25"
                stroke="currentColor"
                stroke-width="1.5"
                stroke-miterlimit="10"
                vector-effect="non-scaling-stroke"
            ></path>
        </svg>
    }
}

#[component]
pub fn Settings(#[prop(into, optional)] class: Signal<String>) -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" fill="currentColor" class=class>
            <path d="M17.966 12.806c.117-.545.06-1.071 0-1.612l1.693-1.21c.162-.08.242-.322.081-.564l-1.612-2.822c-.081-.242-.323-.322-.484-.242l-1.935.887a5.1 5.1 0 0 0-1.451-.806l-.242-2.096c0-.242-.162-.404-.403-.404h-3.225c-.242 0-.404.162-.404.404l-.242 2.015c-.564.242-1.048.484-1.45.806l-1.936-.806a.39.39 0 0 0-.484.162L4.26 9.339c-.161.242-.161.484.08.565l1.694 1.29c-.117.545-.06 1.071 0 1.612l-1.693 1.21c-.162.08-.242.322-.081.564l1.613 2.822c.08.242.322.322.483.242l1.935-.887c.403.322.968.645 1.451.806l.242 2.096c0 .242.162.404.404.404h3.224c.242 0 .404-.162.404-.404l.241-2.096c.565-.242 1.049-.484 1.452-.806l1.935.887c.161.08.403 0 .483-.161l1.613-2.822a.5.5 0 0 0-.08-.565ZM12 15.225A3.234 3.234 0 0 1 8.775 12 3.234 3.234 0 0 1 12 8.775 3.234 3.234 0 0 1 15.225 12 3.234 3.234 0 0 1 12 15.225" />
        </svg>
    }
}

/// Demo debug-panel toggle (info / “i” in circle).
#[component]
pub fn DebugInfo(#[prop(into, optional)] class: Signal<String>) -> impl IntoView {
    view! {
        <svg
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            class=class
            aria-hidden="true"
            focusable="false"
        >
            <circle cx="12" cy="12" r="9" stroke-width="2"></circle>
            <path stroke-width="2" stroke-linecap="round" d="M12 11v6"></path>
            <circle cx="12" cy="8" r="1" fill="currentColor" stroke="none"></circle>
        </svg>
    }
}

#[component]
pub fn Copy(#[prop(into, optional)] class: Signal<String>) -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" fill="currentColor" class=class aria-hidden="true" focusable="false">
            <path d="M16 1H4c-1.1 0-2 .9-2 2v14h2V3h12V1zm3 4H8c-1.1 0-2 .9-2 2v14c0 1.1.9 2 2 2h11c1.1 0 2-.9 2-2V7c0-1.1-.9-2-2-2zm0 16H8V7h11v14z"></path>
        </svg>
    }
}
