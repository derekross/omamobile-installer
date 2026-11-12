// SPDX-License-Identifier: MIT
//! Google's factory image, downloaded for the owner: the terms step, and
//! every property of the download, against a stand-in for Google's server
//! that is a function in this file. No network, no USB.
mod support;
use omamobile_installer_core::build_index::is_download_url;
use omamobile_installer_core::download::{
    FactoryDownload, FetchFailure, FetchNext, FetchRequest, FetchResponse, Mismatch, Unavailable,
    MAX_REDIRECTS,
};
use omamobile_installer_core::flow::{
    DownloadEnd, DownloadTrouble, Flow, FlowOptions, ImageSource, OwnerFileWhy, PhoneFact, Retry,
    Step, Stop, StopKind, CANNOT_FETCH_FROM_GOOGLE, GOOGLE_PRIVACY_POLICY, GOOGLE_TERMS,
    GOOGLE_TERMS_OF_SERVICE,
};
use omamobile_installer_core::Error;
use pollster::block_on;
use support::bundle::{TestBundle, PHONE_BUILD};
use support::fake::{FakePhone, SERIAL};

/// 2026-11-12 14:03:59 UTC.
const NOW: u64 = 1_794_492_239;
const PAGE: &str = "https://developers.google.com/android/images#cheetah";

fn phone() -> FakePhone {
    FakePhone::scenario("ready").unwrap()
}
fn url(bundle: &TestBundle) -> String {
    format!(
        "https://dl.google.com/dl/android/aosp/{}",
        bundle.factory_zip_name
    )
}
/// A flow at the terms step, on a ready phone.
fn at_terms(bundle: &TestBundle, phone: &FakePhone) -> Flow {
    let mut flow = Flow::begin(
        &bundle.files(),
        bundle.index(),
        &bundle.trusted(),
        FlowOptions::default(),
    );
    flow.acknowledge_warning().unwrap();
    let id = omamobile_installer_core::fastboot::PhoneId::of_serial(SERIAL).unwrap();
    block_on(flow.read_phone(&mut phone.clone(), &id)).unwrap();
    assert_eq!(flow.screen().state, "google-terms");
    flow
}
/// The same, with Google's terms accepted: the installer may download.
fn accepted(bundle: &TestBundle, phone: &FakePhone) -> Flow {
    let mut flow = at_terms(bundle, phone);
    flow.accept_google_terms(NOW).unwrap();
    flow
}
fn ok(size: u64) -> FetchResponse<'static> {
    FetchResponse {
        status: 200,
        content_length: Some(size),
        ..Default::default()
    }
}
fn redirect(to: &str) -> FetchResponse<'_> {
    FetchResponse {
        status: 302,
        location: Some(to),
        ..Default::default()
    }
}
fn feed(download: &mut FactoryDownload, bytes: &[u8]) -> bool {
    bytes.chunks(4099).all(|chunk| download.body(chunk))
}
fn stop(flow: &Flow) -> &Stop {
    match flow.step() {
        Step::Stopped(stop) => stop,
        other => panic!("not stopped: {other:?}"),
    }
}
fn source(flow: &Flow) -> ImageSource {
    match flow.step() {
        Step::FactoryImage { source, .. } => source.clone(),
        other => panic!("left the step: {other:?}"),
    }
}
fn trouble(flow: &Flow) -> Option<DownloadTrouble> {
    match flow.step() {
        Step::FactoryImage { trouble, .. } => trouble.clone(),
        other => panic!("left the step: {other:?}"),
    }
}
/// The phone was asked what it is, and nothing else, whatever was downloaded.
fn only_getvar(phone: &FakePhone) {
    assert!(phone.log().iter().all(|c| c.starts_with("getvar:")));
    assert!(phone.written().is_empty());
}

#[test]
fn the_terms_step_shows_googles_words_and_nothing_is_downloaded_before_they_are_accepted() {
    let bundle = TestBundle::new();
    let phone = phone();
    let mut flow = at_terms(&bundle, &phone);
    let screen = flow.screen();
    assert_eq!(
        (screen.state, screen.action),
        ("google-terms", "accept-terms")
    );
    assert_eq!(screen.title, "Google's terms for the factory image");
    // Google's words, as Google's, with the pages they name
    let quote = screen.quote.clone().expect("the terms are quoted");
    assert!(quote
        .intro
        .contains("These are Google's words, not Omarchy Mobile's"));
    assert_eq!(
        quote.lines,
        [
            "Downloading of the system image and use of the device software is subject to the Google Terms of Service",
            "By continuing, you agree to the Google Terms of Service and Privacy Policy"
        ]
    );
    assert_eq!(quote.lines, GOOGLE_TERMS);
    assert_eq!(quote.after, ["Google's page also says that the download and use \"may also be subject to certain third-party terms of service, which can be found in Settings > About phone > Legal information\"."]);
    let links: Vec<(&str, &str)> = quote
        .links
        .iter()
        .map(|l| (l.label.as_str(), l.url.as_str()))
        .collect();
    assert_eq!(
        links,
        [
            (
                "Google Terms of Service",
                "https://policies.google.com/terms"
            ),
            (
                "Google Privacy Policy",
                "https://policies.google.com/privacy"
            )
        ]
    );
    assert_eq!(
        (GOOGLE_TERMS_OF_SERVICE, GOOGLE_PRIVACY_POLICY),
        (links[0].1, links[1].1)
    );
    // Google's page for this build, from the signed index
    assert_eq!(screen.link.as_deref(), Some(PAGE));
    // In our own words: whose the files are, and that nobody passes them on
    let body = screen.body.join(" ");
    assert!(body.contains("Google's page says these files are for use only on your own Pixel, and Google does not allow them to be redistributed."));
    assert!(body.contains("So the installer downloads them straight from Google to this computer, for this phone, and Omarchy Mobile does not host them or pass them on."));
    assert!(body.contains(PHONE_BUILD));
    assert!(body.contains("downloads nothing until you accept"));
    // None of Google's words pass as ours
    for line in GOOGLE_TERMS {
        assert!(!body.contains(line) && !screen.footer.join(" ").contains(line));
    }
    let footer = screen.footer.join(" ");
    assert!(footer.contains(
        "If you do not accept here, the installer downloads nothing, and you can still go on"
    ));
    assert!(footer.contains("It is sent nowhere."));
    // The address of the download is on no screen
    assert!(!serde_json::to_string(&screen)
        .unwrap()
        .contains("dl.google.com"));

    // Nothing happens until the owner answers: no download, no file, no later step
    assert!(flow.google_terms_accepted().is_none());
    assert!(matches!(flow.factory_download(0), Err(Error::Refused(_))));
    assert!(flow
        .factory_zip(&bundle.factory_zip_name, bundle.factory_zip.len() as u64)
        .is_err());
    assert_eq!(flow.screen().state, "google-terms");

    // Accepted: recorded with the build and the time, in the flow, and shown
    flow.accept_google_terms(NOW).unwrap();
    let terms = flow.google_terms_accepted().unwrap();
    assert_eq!((terms.build.as_str(), terms.at), (PHONE_BUILD, NOW));
    assert_eq!(terms.when(), "2026-11-12 at 14:03 UTC");
    assert_eq!(source(&flow), ImageSource::Download);
    let screen = flow.screen();
    assert_eq!((screen.state, screen.action), ("factory-image", "download"));
    let body = screen.body.join(" ");
    assert!(body.contains("You accepted Google's terms for build TEST.260101.001 on 2026-11-12 at 14:03 UTC. The installer keeps that for this run only and sends it nowhere."));
    assert!(body.contains("The installer downloads the image from Google's own server, dl.google.com, to this computer"));
    assert!(body.contains("uses nothing of the file before that"));
    assert!(body.contains("Omarchy Mobile does not host them or pass them on."));
    // Asked once
    assert!(flow.accept_google_terms(NOW).is_err());
    assert!(flow.decline_google_terms().is_err());
    only_getvar(&phone);
}

#[test]
fn the_time_of_an_acceptance_is_a_date_anyone_can_read() {
    use omamobile_installer_core::flow::TermsAccepted;
    for (at, when) in [
        (0, "1970-01-01 at 00:00 UTC"),
        (1_709_251_140, "2024-02-29 at 23:59 UTC"),
        (1_798_761_600, "2027-01-01 at 00:00 UTC"),
        (NOW, "2026-11-12 at 14:03 UTC"),
    ] {
        let terms = TermsAccepted {
            build: PHONE_BUILD.into(),
            at,
        };
        assert_eq!(terms.when(), when);
    }
}

#[test]
fn declining_the_terms_leaves_the_owners_own_download_and_the_installer_fetches_nothing() {
    let bundle = TestBundle::new();
    let phone = phone();
    let mut flow = at_terms(&bundle, &phone);
    flow.decline_google_terms().unwrap();
    assert!(flow.google_terms_accepted().is_none());
    assert_eq!(
        source(&flow),
        ImageSource::OwnerFile(OwnerFileWhy::TermsDeclined)
    );
    let screen = flow.screen();
    assert_eq!(
        (screen.state, screen.action),
        ("factory-image", "take-file")
    );
    assert_eq!(screen.steps.len(), 3);
    assert_eq!(screen.link.as_deref(), Some(PAGE));
    // Not a stop, and no download, now or later in this flow
    assert!(matches!(
        flow.factory_download(0),
        Err(Error::Refused(why)) if why.contains("Google's terms were not accepted")
    ));
    assert!(flow.accept_google_terms(NOW).is_err());
    // The owner's own file is taken as before
    let mut check = flow
        .factory_zip(&bundle.factory_zip_name, bundle.factory_zip.len() as u64)
        .unwrap()
        .unwrap();
    check.update(&bundle.factory_zip);
    flow.factory_zip_done(check).unwrap();
    assert_eq!(flow.step(), &Step::Answers);

    // An owner who said up front that they have the file is not shown the
    // terms: nothing is downloaded, so the installer has none to show
    let mut flow = Flow::begin(
        &bundle.files(),
        bundle.index(),
        &bundle.trusted(),
        FlowOptions {
            owner_has_factory_zip: true,
            ..Default::default()
        },
    );
    flow.acknowledge_warning().unwrap();
    let id = omamobile_installer_core::fastboot::PhoneId::of_serial(SERIAL).unwrap();
    block_on(flow.read_phone(&mut phone.clone(), &id)).unwrap();
    assert_eq!(source(&flow), ImageSource::OwnerFile(OwnerFileWhy::SaidSo));
    let screen = flow.screen();
    assert_eq!(screen.action, "take-file");
    assert!(screen
        .body
        .join(" ")
        .contains("The installer downloads nothing in this run"));
    assert!(matches!(flow.factory_download(0), Err(Error::Refused(_))));
    only_getvar(&phone);
}

#[test]
fn a_front_end_that_cannot_fetch_from_google_is_not_asked_for_googles_terms() {
    // What the owner reads in place of the terms step
    assert_eq!(CANNOT_FETCH_FROM_GOOGLE, "A web page is not allowed to read files from Google's server, so this page cannot download the image for you. The installer's command-line tool can download it for you. Here you download the image from Google's page yourself, where Google asks you to accept its terms, and the installer checks the file you saved.");
    let bundle = TestBundle::new();
    let id = omamobile_installer_core::fastboot::PhoneId::of_serial(SERIAL).unwrap();
    let begin = || {
        let mut flow = Flow::begin(
            &bundle.files(),
            bundle.index(),
            &bundle.trusted(),
            // Said once, at the start, by the front end
            FlowOptions {
                cannot_fetch_from_google: true,
                ..Default::default()
            },
        );
        flow.acknowledge_warning().unwrap();
        flow
    };
    let phone = phone();
    let mut flow = begin();
    block_on(flow.read_phone(&mut phone.clone(), &id)).unwrap();
    // Straight to the owner's own download: no terms for a download that
    // can't be made
    assert_eq!(
        source(&flow),
        ImageSource::OwnerFile(OwnerFileWhy::CannotFetch)
    );
    let screen = flow.screen();
    assert_eq!(
        (screen.state, screen.action),
        ("factory-image", "take-file")
    );
    assert_eq!(screen.quote, None);
    assert!(
        screen.body.contains(&CANNOT_FETCH_FROM_GOOGLE.to_string()),
        "{:?}",
        screen.body
    );
    // Nothing on the screen offers a download or hedges about why
    let text = serde_json::to_string(&screen).unwrap();
    assert!(!text.contains("got no answer"), "{text}");
    assert!(!text.contains("downloads the image"), "{text}");
    assert_eq!(screen.steps.len(), 3);
    assert!(screen.steps[0].text.contains("accept Google's terms there"));
    assert_eq!(screen.link.as_deref(), Some(PAGE));
    // The terms were never asked, can't be answered, and nothing is fetched
    assert!(flow.google_terms_accepted().is_none());
    assert!(flow.accept_google_terms(NOW).is_err());
    assert!(flow.decline_google_terms().is_err());
    assert!(matches!(
        flow.factory_download(0),
        Err(Error::Refused(why)) if why.contains("nothing is downloaded")
    ));
    assert!(flow.google_terms_accepted().is_none());
    // The file the owner saved is checked as ever
    let mut check = flow
        .factory_zip(&bundle.factory_zip_name, bundle.factory_zip.len() as u64)
        .unwrap()
        .unwrap();
    check.update(&bundle.factory_zip);
    flow.factory_zip_done(check).unwrap();
    assert_eq!(flow.step(), &Step::Answers);
    only_getvar(&phone);

    // A locked phone comes the same way, after its owner unlocked it
    let locked = FakePhone::scenario("locked").unwrap();
    let mut flow = begin();
    block_on(flow.read_phone(&mut locked.clone(), &id)).unwrap();
    assert_eq!(flow.screen().state, "unlock");
    locked.edit(|s| {
        s.vars.insert("unlocked".into(), "yes".into());
    });
    block_on(flow.read_phone(&mut locked.clone(), &id)).unwrap();
    assert_eq!(
        source(&flow),
        ImageSource::OwnerFile(OwnerFileWhy::CannotFetch)
    );

    // A front end that can fetch (the default) is shown the terms as before
    let flow = at_terms(&bundle, &phone);
    assert!(!serde_json::to_string(&flow.screen())
        .unwrap()
        .contains("A web page is not allowed"));
}

#[test]
fn the_download_asks_for_the_signed_indexs_address_and_ends_with_the_checked_file() {
    let bundle = TestBundle::new();
    let phone = phone();
    let mut flow = accepted(&bundle, &phone);
    let zip = &bundle.factory_zip;
    let size = zip.len() as u64;
    let mut download = flow.factory_download(0).unwrap();
    assert_eq!(download.resume_from(), 0);
    // The address is the signed index's, whole; the size is the index's
    let request = download.request().unwrap();
    assert_eq!(request.url(), url(&bundle));
    assert_eq!((request.from(), request.size()), (0, size));
    assert_eq!(request.range(), None);
    assert_eq!(download.name(), bundle.factory_zip_name);
    assert_eq!(
        download.response(&ok(size)).unwrap(),
        FetchNext::Body { at: 0 }
    );
    // Progress, as bytes of the total, piece by piece
    assert_eq!(download.progress(), (0, size));
    assert!(download.body(&zip[..1000]));
    assert_eq!(download.progress(), (1000, size));
    assert!(feed(&mut download, &zip[1000..]));
    assert_eq!(download.progress(), (size, size));
    assert_eq!(
        flow.factory_download_done(download, None).unwrap(),
        DownloadEnd::Verified
    );
    assert_eq!(flow.step(), &Step::Answers);
    let taken = flow.factory_zip_verified().unwrap();
    assert_eq!(
        (taken.build.as_str(), taken.name.as_str(), taken.size),
        (PHONE_BUILD, bundle.factory_zip_name.as_str(), size)
    );
    // Taken once
    assert!(flow.factory_download(0).is_err());
    only_getvar(&phone);

    // A server that doesn't say how much it will send: counted all the same
    let mut flow = accepted(&bundle, &phone);
    let mut download = flow.factory_download(0).unwrap();
    let unsized_answer = FetchResponse {
        status: 200,
        ..Default::default()
    };
    assert_eq!(
        download.response(&unsized_answer).unwrap(),
        FetchNext::Body { at: 0 }
    );
    assert!(feed(&mut download, zip));
    assert_eq!(
        flow.factory_download_done(download, None).unwrap(),
        DownloadEnd::Verified
    );

    // A download started by another install is not this one's
    let other_bundle = TestBundle::new();
    let mut other = accepted(&other_bundle, &phone);
    let mut flow = accepted(&bundle, &phone);
    let mut foreign = other.factory_download(0).unwrap();
    foreign.response(&ok(size)).unwrap();
    assert!(feed(&mut foreign, zip));
    assert!(matches!(
        flow.factory_download_done(foreign, None),
        Err(Error::Refused(_))
    ));
    assert_eq!(flow.screen().state, "factory-image");
}

#[test]
fn only_https_on_dl_google_com_is_an_address_to_fetch_from() {
    assert!(is_download_url(
        "https://dl.google.com/dl/android/aosp/cheetah-ab1c.230101.001-factory-0123abcd.zip"
    ));
    for bad in [
        "http://dl.google.com/dl/android/aosp/x.zip",
        "https://dl.google.com",
        "https://dl.google.com/",
        "https://dl.google.com.example.org/x.zip",
        "https://example.org/dl.google.com/x.zip",
        "https://dl.google.com@example.org/x.zip",
        "https://dl.google.com:8443/x.zip",
        "https://dl.google.com/x.zip?to=example.org",
        "https://dl.google.com/x.zip#part",
        "https://dl.google.com/%2e%2e/x.zip",
        "https://dl.google.com/a/../x.zip",
        "https://dl.google.com/\\example.org/x.zip",
        "https://DL.GOOGLE.COM/x.zip",
        "ftp://dl.google.com/x.zip",
        "//dl.google.com/x.zip",
        "/dl/android/aosp/x.zip",
        "",
    ] {
        assert!(!is_download_url(bad), "{bad}");
    }
    // An index whose address is anywhere else is refused whole, so no
    // build of it ever reaches a download (core/tests/build_index.rs has
    // every case); here, that a flow on such a bundle stops before a phone
    for bad in [
        "http://dl.google.com/dl/android/aosp/",
        "https://example.org/dl/android/aosp/",
    ] {
        let bundle = TestBundle::with_index(|index| {
            let zip = index["builds"][2]["zip"].as_str().unwrap().to_string();
            index["builds"][2]["url"] = format!("{bad}{zip}").into();
        });
        let flow = Flow::begin(
            &bundle.files(),
            bundle.index(),
            &bundle.trusted(),
            FlowOptions::default(),
        );
        assert_eq!(stop(&flow).id(), "build-index-refused", "{bad}");
    }
}

#[test]
fn a_redirect_is_followed_on_dl_google_com_only_and_checked_each_time() {
    let bundle = TestBundle::new();
    let phone = phone();
    let zip = &bundle.factory_zip;
    let size = zip.len() as u64;

    // Followed: another path on the same host, as a whole address or a path
    let mut flow = accepted(&bundle, &phone);
    let mut download = flow.factory_download(0).unwrap();
    let next = download
        .response(&redirect("https://dl.google.com/edge/one/file.zip"))
        .unwrap();
    let FetchNext::Fetch(request) = next else {
        panic!("{next:?}")
    };
    assert_eq!(request.url(), "https://dl.google.com/edge/one/file.zip");
    assert_eq!((request.from(), request.size()), (0, size));
    let next = download.response(&redirect("/edge/two/file.zip")).unwrap();
    assert!(
        matches!(&next, FetchNext::Fetch(r) if r.url() == "https://dl.google.com/edge/two/file.zip")
    );
    for status in [301, 303, 307] {
        let answer = FetchResponse {
            status,
            location: Some("/edge/three/file.zip"),
            ..Default::default()
        };
        assert!(matches!(
            download.response(&answer).unwrap(),
            FetchNext::Fetch(_)
        ));
    }
    // And no further than that many in a row
    assert_eq!(MAX_REDIRECTS, 5);
    let sixth = FetchResponse {
        status: 308,
        location: Some("/edge/four/file.zip"),
        ..Default::default()
    };
    assert_eq!(download.response(&sixth).unwrap(), FetchNext::End);
    assert_eq!(
        flow.factory_download_done(download, None).unwrap(),
        DownloadEnd::Kept
    );
    assert_eq!(
        source(&flow),
        ImageSource::OwnerFile(OwnerFileWhy::Unavailable(Unavailable::TooManyRedirects))
    );

    // Not followed: anywhere else, by any spelling, on the first redirect or a later one
    for to in [
        "https://example.org/dl/android/aosp/file.zip",
        "http://dl.google.com/dl/android/aosp/file.zip",
        "https://dl.google.com.example.org/file.zip",
        "https://dl.google.com@example.org/file.zip",
        "https://dl.google.com:8443/file.zip",
        "https://redirector.gvt1.com/edgedl/android/aosp/file.zip",
        "//example.org/file.zip",
        "/\\example.org/file.zip",
        "/dl/../file.zip?x=1",
        "file.zip",
        "",
    ] {
        for hops_before in [0, 2] {
            let mut flow = accepted(&bundle, &phone);
            let mut download = flow.factory_download(0).unwrap();
            for _ in 0..hops_before {
                download
                    .response(&redirect("https://dl.google.com/edge/file.zip"))
                    .unwrap();
            }
            assert_eq!(
                download.response(&redirect(to)).unwrap(),
                FetchNext::End,
                "{to}"
            );
            // The try is over: no request, no body, no second answer
            assert!(download.request().is_none());
            assert!(!download.body(zip));
            assert!(download.response(&ok(size)).is_err());
            assert_eq!(
                flow.factory_download_done(download, None).unwrap(),
                DownloadEnd::Kept
            );
            // Not a stop and not a fault of the file: the owner fetches it by hand
            assert_eq!(
                source(&flow),
                ImageSource::OwnerFile(OwnerFileWhy::Unavailable(Unavailable::OffGoogle)),
                "{to}"
            );
            let screen = flow.screen();
            assert_eq!(
                (screen.state, screen.action),
                ("factory-image", "take-file")
            );
            let body = screen.body.join(" ");
            assert!(body.contains("The installer could not download the image from Google's server: Google's server sent the installer on to another address, and the installer fetches Google's files from https://dl.google.com and nowhere else, so it did not follow."), "{body}");
            assert!(body.contains("That says nothing against the file. You can still go on: download the image from Google's page yourself"));
            // The address it was sent to is not put on the screen
            assert!(!body.contains("example.org") && !body.contains("gvt1"));
            assert_eq!(screen.steps.len(), 3);
            assert!(flow.factory_download(0).is_err());
        }
    }
    // A redirect that names no address
    let mut flow = accepted(&bundle, &phone);
    let mut download = flow.factory_download(0).unwrap();
    let nowhere = FetchResponse {
        status: 302,
        ..Default::default()
    };
    assert_eq!(download.response(&nowhere).unwrap(), FetchNext::End);
    flow.factory_download_done(download, None).unwrap();
    assert_eq!(
        source(&flow),
        ImageSource::OwnerFile(OwnerFileWhy::Unavailable(Unavailable::BadAnswer))
    );

    // A client that follows redirects itself (a browser) says where the
    // answer came from in the end, and that is checked the same way
    let mut flow = accepted(&bundle, &phone);
    let mut download = flow.factory_download(0).unwrap();
    let elsewhere = FetchResponse {
        final_url: Some("https://example.org/file.zip"),
        ..ok(size)
    };
    assert_eq!(download.response(&elsewhere).unwrap(), FetchNext::End);
    flow.factory_download_done(download, None).unwrap();
    assert_eq!(
        source(&flow),
        ImageSource::OwnerFile(OwnerFileWhy::Unavailable(Unavailable::OffGoogle))
    );
    let mut flow = accepted(&bundle, &phone);
    let mut download = flow.factory_download(0).unwrap();
    let same_host = FetchResponse {
        final_url: Some("https://dl.google.com/edge/file.zip"),
        ..ok(size)
    };
    assert_eq!(
        download.response(&same_host).unwrap(),
        FetchNext::Body { at: 0 }
    );

    // After the fallback the owner's own file is taken, checked as ever
    let mut check = flow
        .factory_zip(&bundle.factory_zip_name, size)
        .unwrap()
        .unwrap();
    check.update(zip);
    flow.factory_zip_done(check).unwrap();
    assert_eq!(flow.step(), &Step::Answers);
    only_getvar(&phone);
}

fn why_of(stop: &Stop) -> Mismatch {
    match &stop.kind {
        StopKind::FactoryDownloadMismatch { why, .. } => why.clone(),
        other => panic!("another stop: {other:?}"),
    }
}

/// The stop for a download that is not the listed file: typed, said in the
/// words the task of the stop is, and with nothing reassuring past the facts.
fn mismatch_stop(flow: &Flow, why: Mismatch, says: &str) {
    let stop = stop(flow);
    assert_eq!(stop.id(), "factory-download-mismatch");
    assert_eq!(
        stop.kind,
        StopKind::FactoryDownloadMismatch {
            build: PHONE_BUILD.into(),
            why
        }
    );
    assert_eq!(
        stop.title,
        "The downloaded file is not the one this release lists"
    );
    let message = stop.message.join(" ");
    assert!(message.contains(says), "{message}");
    assert!(message.contains("not what the release's signed list of Google's builds says"));
    if matches!(why_of(stop), Mismatch::AnnouncedSize { .. }) {
        // Nothing was taken, so nothing is said to be deleted
        assert!(message.contains("The installer took nothing of what the server offered."));
        assert!(!message.contains("deleted"), "{message}");
        assert!(!message.contains("afresh"), "{message}");
    } else {
        assert!(message.contains("The installer uses nothing of that file, and what it had downloaded of it is deleted from this computer."));
        assert!(message.contains("Starting the installer again downloads the file afresh."));
    }
    assert!(message.contains("https://github.com/derekross/omarchy-mobile/issues"));
    assert_eq!(
        (stop.retry, stop.phone),
        (Retry::Whenever, PhoneFact::OnlyRead)
    );
    let footer = stop.footer().join(" ");
    assert!(footer.contains("It has written nothing to it.") && footer.contains("at any time"));
    assert_eq!(flow.screen().stop, Some("factory-download-mismatch"));
    assert!(flow.factory_zip_verified().is_none());
}

#[test]
fn the_size_is_the_indexs_before_the_transfer_and_during_it() {
    let bundle = TestBundle::new();
    let phone = phone();
    let zip = &bundle.factory_zip;
    let size = zip.len() as u64;

    // Before: the server says it will send another size. No body is read
    for announced in [size + 1, size - 1, 0, 3_900_000_000] {
        let mut flow = accepted(&bundle, &phone);
        let mut download = flow.factory_download(0).unwrap();
        assert_eq!(download.response(&ok(announced)).unwrap(), FetchNext::End);
        assert!(!download.body(zip));
        assert_eq!(download.progress(), (0, size));
        // Nothing was taken and nothing was kept: there is nothing to delete
        assert_eq!(
            flow.factory_download_done(download, None).unwrap(),
            DownloadEnd::Kept
        );
        mismatch_stop(
            &flow,
            Mismatch::AnnouncedSize { got: announced },
            &format!("Google's server offered a file of {announced} bytes"),
        );
        assert!(stop(&flow)
            .message
            .join(" ")
            .contains("Starting the installer again tries the download again."));
        // Stopped: nothing more is accepted
        assert!(flow.factory_download(0).is_err());
        assert!(flow.factory_zip(&bundle.factory_zip_name, size).is_err());
    }

    // During: more arrives than the file has. Reading stops at the piece
    // that would pass the size, and none of that piece is taken
    for (announced, extra) in [(Some(size), 1usize), (None, 1), (None, 5000)] {
        let mut flow = accepted(&bundle, &phone);
        let mut download = flow.factory_download(0).unwrap();
        let answer = FetchResponse {
            status: 200,
            content_length: announced,
            ..Default::default()
        };
        assert_eq!(
            download.response(&answer).unwrap(),
            FetchNext::Body { at: 0 }
        );
        assert!(feed(&mut download, zip));
        assert!(!download.body(&vec![0u8; extra]));
        assert_eq!(download.progress(), (size, size));
        // And nothing after it either
        assert!(!download.body(&[]));
        assert_eq!(
            flow.factory_download_done(download, None).unwrap(),
            DownloadEnd::Discard
        );
        mismatch_stop(
            &flow,
            Mismatch::TooManyBytes,
            "Google's server sent more for",
        );
    }
    // One piece that straddles the end is refused whole
    let mut flow = accepted(&bundle, &phone);
    let mut download = flow.factory_download(0).unwrap();
    download.response(&ok(size)).unwrap();
    assert!(download.body(&zip[..zip.len() - 10]));
    assert!(!download.body(&[0u8; 11]));
    assert_eq!(download.progress(), (size - 10, size));
    assert_eq!(
        flow.factory_download_done(download, None).unwrap(),
        DownloadEnd::Discard
    );
    mismatch_stop(
        &flow,
        Mismatch::TooManyBytes,
        "the installer took no more than that",
    );
    only_getvar(&phone);
}

#[test]
fn the_sha256_is_checked_over_the_whole_file_and_a_mismatch_stops_and_discards() {
    let bundle = TestBundle::new();
    let phone = phone();
    let zip = &bundle.factory_zip;
    let size = zip.len() as u64;
    // One bit wrong, at the start, in the middle and at the end
    for at in [0, zip.len() / 2, zip.len() - 1] {
        let mut wrong = zip.clone();
        wrong[at] ^= 1;
        let mut flow = accepted(&bundle, &phone);
        let mut download = flow.factory_download(0).unwrap();
        download.response(&ok(size)).unwrap();
        assert!(feed(&mut download, &wrong));
        assert_eq!(
            flow.factory_download_done(download, None).unwrap(),
            DownloadEnd::Discard,
            "{at}"
        );
        mismatch_stop(
            &flow,
            Mismatch::Sha256,
            &format!(
                "The installer downloaded {} from Google's server, and the file is not what the release's signed list of Google's builds says: its SHA-256 is another",
                bundle.factory_zip_name
            ),
        );
        // No later step opened on it
        assert!(flow.screen().state == "stopped");
    }
    // After the unlock steps were shown, the stop says that too
    let locked = FakePhone::scenario("locked").unwrap();
    let mut flow = Flow::begin(
        &bundle.files(),
        bundle.index(),
        &bundle.trusted(),
        FlowOptions::default(),
    );
    flow.acknowledge_warning().unwrap();
    let id = omamobile_installer_core::fastboot::PhoneId::of_serial(SERIAL).unwrap();
    block_on(flow.read_phone(&mut locked.clone(), &id)).unwrap();
    locked.edit(|s| {
        s.vars.insert("unlocked".into(), "yes".into());
    });
    block_on(flow.read_phone(&mut locked.clone(), &id)).unwrap();
    flow.accept_google_terms(NOW).unwrap();
    let mut download = flow.factory_download(0).unwrap();
    download.response(&ok(size)).unwrap();
    assert!(feed(&mut download, &vec![7u8; zip.len()]));
    assert_eq!(
        flow.factory_download_done(download, None).unwrap(),
        DownloadEnd::Discard
    );
    let stop = stop(&flow);
    assert_eq!(stop.phone, PhoneFact::OnlyReadUnlockShown);
    assert!(stop.footer().join(" ").contains("that erased the phone"));
    only_getvar(&phone);
}

/// A try that breaks off after `arrive` bytes; the bytes that arrived.
fn interrupted(flow: &mut Flow, zip: &[u8], have: &[u8], arrive: usize) -> Vec<u8> {
    let mut download = flow.factory_download(have.len() as u64).unwrap();
    download.kept(have).unwrap();
    let answer = if have.is_empty() {
        ok(zip.len() as u64)
    } else {
        FetchResponse {
            status: 206,
            ..Default::default()
        }
    };
    let range = format!("bytes {}-{}/{}", have.len(), zip.len() - 1, zip.len());
    let answer = FetchResponse {
        content_range: (!have.is_empty()).then_some(range.as_str()),
        ..answer
    };
    assert_eq!(
        download.response(&answer).unwrap(),
        FetchNext::Body {
            at: have.len() as u64
        }
    );
    let end = have.len() + arrive;
    assert!(feed(&mut download, &zip[have.len()..end]));
    assert_eq!(
        flow.factory_download_done(
            download,
            Some(FetchFailure::Network("the connection was reset".into()))
        )
        .unwrap(),
        DownloadEnd::Kept
    );
    zip[..end].to_vec()
}

#[test]
fn an_interrupted_download_goes_on_from_what_was_kept_and_is_hashed_from_the_first_byte() {
    let bundle = TestBundle::new();
    let phone = phone();
    let zip = &bundle.factory_zip;
    let size = zip.len() as u64;
    let mut flow = accepted(&bundle, &phone);

    // The connection breaks: the step stays open and says how far it came
    let kept = interrupted(&mut flow, zip, &[], 10_000);
    assert_eq!(
        trouble(&flow),
        Some(DownloadTrouble::Interrupted {
            got: 10_000,
            want: size,
            detail: Some("the connection was reset".into())
        })
    );
    let screen = flow.screen();
    assert_eq!((screen.state, screen.action), ("factory-image", "download"));
    assert_eq!(
        screen.problem.as_deref(),
        Some(format!("The download from Google's server stopped before it was complete: 10000 of {size} bytes had arrived (the connection was reset). You can try again.").as_str())
    );
    // And again, further on
    let kept = interrupted(&mut flow, zip, &kept, 20_000);
    assert_eq!(kept.len(), 30_000);

    // The next try asks for the rest only
    let mut download = flow.factory_download(kept.len() as u64).unwrap();
    assert!(flow.screen().problem.is_none());
    assert_eq!(download.resume_from(), 30_000);
    let request = download.request().unwrap();
    assert_eq!(request.url(), url(&bundle));
    assert_eq!((request.from(), request.size()), (30_000, size));
    assert_eq!(request.range().as_deref(), Some("bytes=30000-"));
    let rest_range = format!("bytes 30000-{}/{size}", size - 1);
    let rest = FetchResponse {
        status: 206,
        content_length: Some(size - 30_000),
        content_range: Some(&rest_range),
        ..Default::default()
    };
    // No answer is taken before every kept byte was handed in: the hash
    // starts at the file's first byte, not at the byte the download went on from
    assert!(matches!(download.response(&rest), Err(Error::Refused(_))));
    download.kept(&kept[..12_345]).unwrap();
    assert!(matches!(download.response(&rest), Err(Error::Refused(_))));
    download.kept(&kept[12_345..]).unwrap();
    // Not more than was said to be kept
    assert!(download.kept(&[0]).is_err());
    assert_eq!(download.progress(), (30_000, size));
    assert_eq!(
        download.response(&rest).unwrap(),
        FetchNext::Body { at: 30_000 }
    );
    assert!(download.kept(&[]).is_err(), "the kept bytes come first");
    assert!(feed(&mut download, &zip[30_000..]));
    assert_eq!(
        flow.factory_download_done(download, None).unwrap(),
        DownloadEnd::Verified
    );
    assert_eq!(flow.step(), &Step::Answers);

    // Kept bytes that are not the file's: the rest is right, the whole is
    // not, and that is found because the kept bytes are hashed too
    let mut flow = accepted(&bundle, &phone);
    let mut spoiled = interrupted(&mut flow, zip, &[], 30_000);
    spoiled[17] ^= 1;
    let mut download = flow.factory_download(30_000).unwrap();
    download.kept(&spoiled).unwrap();
    assert_eq!(
        download.response(&rest).unwrap(),
        FetchNext::Body { at: 30_000 }
    );
    assert!(feed(&mut download, &zip[30_000..]));
    assert_eq!(
        flow.factory_download_done(download, None).unwrap(),
        DownloadEnd::Discard
    );
    mismatch_stop(&flow, Mismatch::Sha256, "its SHA-256 is another");

    // A front end that hands in none of the kept bytes gets no verdict
    let mut flow = accepted(&bundle, &phone);
    let download = flow.factory_download(30_000).unwrap();
    assert!(matches!(
        flow.factory_download_done(download, None),
        Err(Error::Refused(_))
    ));
    assert_eq!(flow.screen().state, "factory-image");

    // A server that sends the whole file although the rest was asked for:
    // what was kept is dropped, and with it its part of the hash
    let mut download = flow.factory_download(30_000).unwrap();
    download.kept(&spoiled).unwrap();
    assert_eq!(
        download.response(&ok(size)).unwrap(),
        FetchNext::Body { at: 0 }
    );
    assert_eq!(download.progress(), (0, size));
    assert!(feed(&mut download, zip));
    assert_eq!(
        flow.factory_download_done(download, None).unwrap(),
        DownloadEnd::Verified
    );

    // An answer for another part, or of another file's size
    for (range, length, want) in [
        (
            format!("bytes 0-{}/{size}", size - 1),
            None,
            Err(Unavailable::BadAnswer),
        ),
        (
            format!("bytes 30000-{}/{size}", size - 2),
            None,
            Err(Unavailable::BadAnswer),
        ),
        (
            format!("bytes 30000-{}/{size}", size - 1),
            Some(5),
            Err(Unavailable::BadAnswer),
        ),
        (
            "bytes */30000".to_string(),
            None,
            Err(Unavailable::BadAnswer),
        ),
        ("".to_string(), None, Err(Unavailable::BadAnswer)),
        (
            format!("bytes 30000-{size}/{}", size + 1),
            None,
            Ok(Mismatch::AnnouncedSize { got: size + 1 }),
        ),
    ] {
        let mut flow = accepted(&bundle, &phone);
        let mut download = flow.factory_download(30_000).unwrap();
        download.kept(&kept).unwrap();
        let answer = FetchResponse {
            status: 206,
            content_length: length,
            content_range: Some(&range),
            ..Default::default()
        };
        assert_eq!(
            download.response(&answer).unwrap(),
            FetchNext::End,
            "{range}"
        );
        let end = flow.factory_download_done(download, None).unwrap();
        match want {
            Err(why) => {
                assert_eq!(end, DownloadEnd::Kept);
                assert_eq!(
                    source(&flow),
                    ImageSource::OwnerFile(OwnerFileWhy::Unavailable(why))
                );
            }
            // Another size said: the install stops, and the bytes an
            // earlier try kept are not deleted for it
            Ok(why) => {
                assert_eq!(end, DownloadEnd::Kept);
                mismatch_stop(&flow, why, "Google's server offered a file of");
            }
        }
    }
    // A part where the whole was asked for
    let mut flow = accepted(&bundle, &phone);
    let mut download = flow.factory_download(0).unwrap();
    let part = FetchResponse {
        status: 206,
        content_range: Some("bytes 0-9/10"),
        ..Default::default()
    };
    assert_eq!(download.response(&part).unwrap(), FetchNext::End);

    // Kept more than the file has: none of it is used
    let mut flow = accepted(&bundle, &phone);
    let download = flow.factory_download(size + 1).unwrap();
    assert_eq!(download.resume_from(), 0);
    assert_eq!(download.request().unwrap().range(), None);

    // Every byte was kept (an earlier run finished the transfer): nothing
    // is fetched, and the file is still hashed whole before it is used
    let mut download = flow.factory_download(size).unwrap();
    assert_eq!(download.resume_from(), size);
    assert!(download.request().is_none());
    assert!(download.response(&ok(size)).is_err());
    download.kept(zip).unwrap();
    assert_eq!(
        flow.factory_download_done(download, None).unwrap(),
        DownloadEnd::Verified
    );
    let mut flow = accepted(&bundle, &phone);
    let mut download = flow.factory_download(size).unwrap();
    let mut wrong = zip.clone();
    wrong[5] ^= 1;
    download.kept(&wrong).unwrap();
    assert_eq!(
        flow.factory_download_done(download, None).unwrap(),
        DownloadEnd::Discard
    );
    mismatch_stop(&flow, Mismatch::Sha256, "its SHA-256 is another");
    only_getvar(&phone);
}

/// The rule for what an earlier try left on disk: it is deleted only when
/// it cannot be part of the right file. What a server says it is about to
/// send is no such proof.
#[test]
fn another_size_said_by_the_server_does_not_delete_what_an_earlier_try_kept() {
    let bundle = TestBundle::new();
    let phone = phone();
    let zip = &bundle.factory_zip;
    let size = zip.len() as u64;
    let kept = &zip[..20_000];
    let rest_one_too_many = format!("bytes 20000-{size}/{}", size + 1);
    let answers = [
        // The whole file, one byte too long
        ok(size + 1),
        // A short page with status 200 in the file's place
        ok(35),
        // The rest, of a file one byte too long
        FetchResponse {
            status: 206,
            content_range: Some(&rest_one_too_many),
            ..Default::default()
        },
    ];
    for answer in &answers {
        let mut flow = accepted(&bundle, &phone);
        let mut download = flow.factory_download(20_000).unwrap();
        download.kept(kept).unwrap();
        assert_eq!(download.response(answer).unwrap(), FetchNext::End);
        // No byte of that answer's body is taken
        assert!(!download.body(&zip[20_000..]));
        assert_eq!(download.progress(), (20_000, size));
        // Kept, not Discard: the front end leaves the bytes where they are
        assert_eq!(
            flow.factory_download_done(download, None).unwrap(),
            DownloadEnd::Kept,
            "{answer:?}"
        );
        // It is still not the listed file, and the install still stops
        let stop = stop(&flow);
        assert_eq!(stop.id(), "factory-download-mismatch");
        assert!(matches!(why_of(stop), Mismatch::AnnouncedSize { .. }));
        let message = stop.message.join(" ");
        assert!(message.contains("The installer took nothing of what the server offered. The 20000 bytes an earlier try downloaded stay on this computer: the server's answer says nothing about them, and they are checked from the first byte, together with the rest, before anything of the file is used."), "{message}");
        assert!(
            message.contains("Starting the installer again goes on from those bytes."),
            "{message}"
        );
        assert!(!message.contains("deleted"), "{message}");

        // The next run goes on from them, and the whole file passes
        let mut flow = accepted(&bundle, &phone);
        let mut download = flow.factory_download(20_000).unwrap();
        download.kept(kept).unwrap();
        let range = format!("bytes 20000-{}/{size}", size - 1);
        let rest = FetchResponse {
            status: 206,
            content_range: Some(&range),
            ..Default::default()
        };
        assert_eq!(
            download.response(&rest).unwrap(),
            FetchNext::Body { at: 20_000 }
        );
        assert!(feed(&mut download, &zip[20_000..]));
        assert_eq!(
            flow.factory_download_done(download, None).unwrap(),
            DownloadEnd::Verified
        );
    }

    // What can't belong to the right file is still deleted. More bytes than
    // the file has: not used, and the front end starts the file afresh
    let mut flow = accepted(&bundle, &phone);
    let download = flow.factory_download(size + 1).unwrap();
    assert_eq!(download.resume_from(), 0);
    // Complete, with another SHA-256
    let mut wrong = zip.clone();
    wrong[9] ^= 1;
    let mut download = flow.factory_download(size).unwrap();
    download.kept(&wrong).unwrap();
    assert_eq!(
        flow.factory_download_done(download, None).unwrap(),
        DownloadEnd::Discard
    );
    // Kept bytes followed by more than the file has
    let mut flow = accepted(&bundle, &phone);
    let mut download = flow.factory_download(20_000).unwrap();
    download.kept(kept).unwrap();
    let range = format!("bytes 20000-{}/{size}", size - 1);
    let rest = FetchResponse {
        status: 206,
        content_range: Some(&range),
        ..Default::default()
    };
    download.response(&rest).unwrap();
    assert!(feed(&mut download, &zip[20_000..]));
    assert!(!download.body(&[0u8; 1]));
    assert_eq!(
        flow.factory_download_done(download, None).unwrap(),
        DownloadEnd::Discard
    );
    only_getvar(&phone);
}

#[test]
fn a_download_that_stalls_is_an_interrupted_one_that_keeps_its_bytes_and_can_be_tried_again() {
    let bundle = TestBundle::new();
    let phone = phone();
    let zip = &bundle.factory_zip;
    let size = zip.len() as u64;
    let mut flow = accepted(&bundle, &phone);
    let mut download = flow.factory_download(0).unwrap();
    download.response(&ok(size)).unwrap();
    assert!(feed(&mut download, &zip[..10_000]));
    // The front end waited a minute for the next byte, and gave up
    assert_eq!(
        flow.factory_download_done(download, Some(FetchFailure::Stalled { seconds: 60 }))
            .unwrap(),
        DownloadEnd::Kept
    );
    assert_eq!(
        trouble(&flow),
        Some(DownloadTrouble::Interrupted {
            got: 10_000,
            want: size,
            detail: Some("Google's server sent nothing for 60 seconds".into()),
        })
    );
    // Not a stop and not a fallback: the download is still on offer
    assert_eq!(source(&flow), ImageSource::Download);
    let screen = flow.screen();
    assert_eq!((screen.state, screen.action), ("factory-image", "download"));
    assert_eq!(
        screen.problem.as_deref(),
        Some(format!("The download from Google's server stopped before it was complete: 10000 of {size} bytes had arrived (Google's server sent nothing for 60 seconds). You can try again.").as_str())
    );
    // The next try goes on from what was kept, hashed from the first byte
    let mut download = flow.factory_download(10_000).unwrap();
    download.kept(&zip[..10_000]).unwrap();
    assert_eq!(
        download.request().unwrap().range().as_deref(),
        Some("bytes=10000-")
    );
    let range = format!("bytes 10000-{}/{size}", size - 1);
    let answer = FetchResponse {
        status: 206,
        content_range: Some(&range),
        ..Default::default()
    };
    assert_eq!(
        download.response(&answer).unwrap(),
        FetchNext::Body { at: 10_000 }
    );
    assert!(feed(&mut download, &zip[10_000..]));
    assert_eq!(
        flow.factory_download_done(download, None).unwrap(),
        DownloadEnd::Verified
    );
    assert_eq!(flow.step(), &Step::Answers);

    // Silence before the first byte of the body is the same, with nothing kept
    let mut flow = accepted(&bundle, &phone);
    let mut download = flow.factory_download(0).unwrap();
    download.response(&ok(size)).unwrap();
    assert_eq!(
        flow.factory_download_done(download, Some(FetchFailure::Stalled { seconds: 60 }))
            .unwrap(),
        DownloadEnd::Kept
    );
    assert!(flow.screen().problem.unwrap().contains(&format!(
        "0 of {size} bytes had arrived (Google's server sent nothing for 60 seconds)"
    )));
    only_getvar(&phone);
}

#[test]
fn what_a_server_or_an_http_client_says_is_made_fit_to_print() {
    let bundle = TestBundle::new();
    let phone = phone();
    let size = bundle.factory_zip.len() as u64;
    // An ESC sequence, a carriage return, a NUL and a bell, as a server
    // could put them in a header that an HTTP client then quotes
    let hostile = "bad header \u{1b}[2J\u{1b}]0;gone\u{7}\rall is well\0";
    for failure in [
        FetchFailure::Network(hostile.into()),
        FetchFailure::Storage(hostile.into()),
    ] {
        let mut flow = accepted(&bundle, &phone);
        let mut download = flow.factory_download(0).unwrap();
        download.response(&ok(size)).unwrap();
        flow.factory_download_done(download, Some(failure)).unwrap();
        let screen = flow.screen();
        let problem = screen.problem.unwrap();
        assert!(!problem.chars().any(char::is_control), "{problem:?}");
        assert!(
            problem.contains("bad header \\u{1b}[2J\\u{1b}]0;gone\\u{7} all is well\\u{0}"),
            "{problem}"
        );
    }
}

#[test]
fn a_download_that_cannot_be_made_falls_back_to_the_owners_own_download() {
    let bundle = TestBundle::new();
    let phone = phone();
    let zip = &bundle.factory_zip;
    let size = zip.len() as u64;

    // A browser whose fetch was rejected before any answer: a page is not
    // told whether that was the network or the server's cross-origin rule
    let mut flow = accepted(&bundle, &phone);
    let download = flow.factory_download(0).unwrap();
    assert_eq!(
        flow.factory_download_done(download, Some(FetchFailure::NoAnswer))
            .unwrap(),
        DownloadEnd::Kept
    );
    assert_eq!(
        source(&flow),
        ImageSource::OwnerFile(OwnerFileWhy::Unavailable(Unavailable::NoAnswer))
    );
    let screen = flow.screen();
    assert_eq!(
        (screen.state, screen.action),
        ("factory-image", "take-file")
    );
    let body = screen.body.join(" ");
    assert!(body.contains("The installer could not download the image from Google's server: this browser got no answer from dl.google.com that a web page may read. A browser does not tell a page why: either there is no connection to Google's server, or Google's server does not let a web page read its files."), "{body}");
    assert!(body.contains("You can still go on: download the image from Google's page yourself, and the installer checks the file you saved."));
    assert_eq!(screen.link.as_deref(), Some(PAGE));
    assert_eq!(screen.steps.len(), 3);
    assert!(screen.problem.is_none());
    // The acceptance stays on record; the installer downloads no more
    assert!(flow.google_terms_accepted().is_some());
    assert!(flow.factory_download(0).is_err());

    // Neither the file nor a redirect
    for status in [403, 404, 416, 429, 500, 503, 204, 304] {
        let mut flow = accepted(&bundle, &phone);
        let mut download = flow.factory_download(0).unwrap();
        let answer = FetchResponse {
            status,
            ..Default::default()
        };
        assert_eq!(download.response(&answer).unwrap(), FetchNext::End);
        assert_eq!(
            flow.factory_download_done(download, None).unwrap(),
            DownloadEnd::Kept
        );
        assert_eq!(
            source(&flow),
            ImageSource::OwnerFile(OwnerFileWhy::Unavailable(Unavailable::Status(status)))
        );
        assert!(flow.screen().body.join(" ").contains(&format!(
            "Google's server answered with HTTP status {status} instead of the file."
        )));
    }

    // Troubles that leave the download open, to try again
    let mut flow = accepted(&bundle, &phone);
    let download = flow.factory_download(0).unwrap();
    assert_eq!(
        flow.factory_download_done(
            download,
            Some(FetchFailure::Storage("the disk is full".into()))
        )
        .unwrap(),
        DownloadEnd::Kept
    );
    assert_eq!(
        trouble(&flow),
        Some(DownloadTrouble::Storage("the disk is full".into()))
    );
    assert_eq!(flow.screen().action, "download");
    assert_eq!(
        flow.screen().problem.as_deref(),
        Some("The installer could not keep the download on this computer (the disk is full). You can try again once that is put right.")
    );
    // The server closes the connection early without an error
    let mut download = flow.factory_download(0).unwrap();
    download.response(&ok(size)).unwrap();
    assert!(feed(&mut download, &zip[..777]));
    assert_eq!(
        flow.factory_download_done(download, None).unwrap(),
        DownloadEnd::Kept
    );
    assert_eq!(
        trouble(&flow),
        Some(DownloadTrouble::Interrupted {
            got: 777,
            want: size,
            detail: None
        })
    );
    assert_eq!(
        flow.screen().problem.as_deref(),
        Some(format!("The download from Google's server stopped before it was complete: 777 of {size} bytes had arrived. You can try again.").as_str())
    );
    // While the download is open, a file the owner hands in is taken too,
    // and a file that isn't taken says so in place of the download's trouble
    assert!(flow.factory_zip("other.zip", size).unwrap().is_none());
    assert!(flow.screen().problem.unwrap().contains("is not the file"));
    assert_eq!(trouble(&flow), None);
    let mut check = flow
        .factory_zip(&bundle.factory_zip_name, size)
        .unwrap()
        .unwrap();
    check.update(zip);
    flow.factory_zip_done(check).unwrap();
    assert_eq!(flow.step(), &Step::Answers);
    only_getvar(&phone);
}

/// A `FetchRequest` has no constructor outside the core: what a front end
/// fetches can only be what the core handed it.
#[test]
fn a_request_comes_from_the_core_and_carries_the_indexs_address_and_size() {
    let bundle = TestBundle::new();
    let phone = phone();
    let mut flow = accepted(&bundle, &phone);
    let download = flow.factory_download(0).unwrap();
    let request: FetchRequest = download.request().unwrap();
    let index: serde_json::Value = serde_json::from_slice(bundle.index().unwrap()).unwrap();
    assert_eq!(request.url(), index["builds"][2]["url"].as_str().unwrap());
    assert_eq!(request.size(), index["builds"][2]["size"].as_u64().unwrap());
    assert!(is_download_url(request.url()));
}
