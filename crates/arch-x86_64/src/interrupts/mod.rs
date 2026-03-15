mod handlers;

use crate::cpu::{InterruptDescriptorTable, IdtEntry};
use handlers::*;

/// Monotonically increasing tick counter — incremented by the timer ISR at 100 Hz.
pub static TICK_COUNT: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

pub use handlers::MAX_ISR_LATENCY;

/// Wire all 256 IDT vectors.
pub fn init(idt: &mut InterruptDescriptorTable) {
    // ── Dedicated exception handlers ──────────────────────────────────────────
    idt.set_entry( 0, IdtEntry::interrupt_gate(divide_by_zero_handler          as *const () as u64, 0x08));
    idt.set_entry( 2, IdtEntry::interrupt_gate_ist(nmi_handler                  as *const () as u64, 0x08, 2));
    idt.set_entry( 8, IdtEntry::interrupt_gate_ist(double_fault_handler         as *const () as u64, 0x08, 1));
    idt.set_entry(13, IdtEntry::interrupt_gate(general_protection_fault_handler as *const () as u64, 0x08));
    idt.set_entry(14, IdtEntry::interrupt_gate(page_fault_handler               as *const () as u64, 0x08));
    idt.set_entry(18, IdtEntry::interrupt_gate_ist(machine_check_handler        as *const () as u64, 0x08, 3));
    idt.set_entry(32, IdtEntry::interrupt_gate(timer_interrupt_handler          as *const () as u64, 0x08));
    idt.set_entry(255, IdtEntry::interrupt_gate(spurious_handler                as *const () as u64, 0x08));

    // ── Catch-all for every other vector ─────────────────────────────────────
    macro_rules! fill {
        ($idx:expr, $stub:ident) => {
            idt.set_entry_usize($idx, IdtEntry::interrupt_gate($stub as *const () as u64, 0x08));
        };
    }
    fill!(  1, unexpected_vec1);   fill!(  3, unexpected_vec3);
    fill!(  4, unexpected_vec4);   fill!(  5, unexpected_vec5);
    fill!(  6, unexpected_vec6);   fill!(  7, unexpected_vec7);
    fill!(  9, unexpected_vec9);   fill!( 10, unexpected_vec10);
    fill!( 11, unexpected_vec11);  fill!( 12, unexpected_vec12);
    fill!( 15, unexpected_vec15);  fill!( 16, unexpected_vec16);
    fill!( 17, unexpected_vec17);  fill!( 19, unexpected_vec19);
    fill!( 20, unexpected_vec20);  fill!( 21, unexpected_vec21);
    fill!( 22, unexpected_vec22);  fill!( 23, unexpected_vec23);
    fill!( 24, unexpected_vec24);  fill!( 25, unexpected_vec25);
    fill!( 26, unexpected_vec26);  fill!( 27, unexpected_vec27);
    fill!( 28, unexpected_vec28);  fill!( 29, unexpected_vec29);
    fill!( 30, unexpected_vec30);  fill!( 31, unexpected_vec31);
    fill!( 33, unexpected_vec33);  fill!( 34, unexpected_vec34);
    fill!( 35, unexpected_vec35);  fill!( 36, unexpected_vec36);
    fill!( 37, unexpected_vec37);  fill!( 38, unexpected_vec38);
    fill!( 39, unexpected_vec39);  fill!( 40, unexpected_vec40);
    fill!( 41, unexpected_vec41);  fill!( 42, unexpected_vec42);
    fill!( 43, unexpected_vec43);  fill!( 44, unexpected_vec44);
    fill!( 45, unexpected_vec45);  fill!( 46, unexpected_vec46);
    fill!( 47, unexpected_vec47);
    fill!( 48, unexpected_vec48);  fill!( 49, unexpected_vec49);
    fill!( 50, unexpected_vec50);  fill!( 51, unexpected_vec51);
    fill!( 52, unexpected_vec52);  fill!( 53, unexpected_vec53);
    fill!( 54, unexpected_vec54);  fill!( 55, unexpected_vec55);
    fill!( 56, unexpected_vec56);  fill!( 57, unexpected_vec57);
    fill!( 58, unexpected_vec58);  fill!( 59, unexpected_vec59);
    fill!( 60, unexpected_vec60);  fill!( 61, unexpected_vec61);
    fill!( 62, unexpected_vec62);  fill!( 63, unexpected_vec63);
    fill!( 64, unexpected_vec64);  fill!( 65, unexpected_vec65);
    fill!( 66, unexpected_vec66);  fill!( 67, unexpected_vec67);
    fill!( 68, unexpected_vec68);  fill!( 69, unexpected_vec69);
    fill!( 70, unexpected_vec70);  fill!( 71, unexpected_vec71);
    fill!( 72, unexpected_vec72);  fill!( 73, unexpected_vec73);
    fill!( 74, unexpected_vec74);  fill!( 75, unexpected_vec75);
    fill!( 76, unexpected_vec76);  fill!( 77, unexpected_vec77);
    fill!( 78, unexpected_vec78);  fill!( 79, unexpected_vec79);
    fill!( 80, unexpected_vec80);  fill!( 81, unexpected_vec81);
    fill!( 82, unexpected_vec82);  fill!( 83, unexpected_vec83);
    fill!( 84, unexpected_vec84);  fill!( 85, unexpected_vec85);
    fill!( 86, unexpected_vec86);  fill!( 87, unexpected_vec87);
    fill!( 88, unexpected_vec88);  fill!( 89, unexpected_vec89);
    fill!( 90, unexpected_vec90);  fill!( 91, unexpected_vec91);
    fill!( 92, unexpected_vec92);  fill!( 93, unexpected_vec93);
    fill!( 94, unexpected_vec94);  fill!( 95, unexpected_vec95);
    fill!( 96, unexpected_vec96);  fill!( 97, unexpected_vec97);
    fill!( 98, unexpected_vec98);  fill!( 99, unexpected_vec99);
    fill!(100, unexpected_vec100); fill!(101, unexpected_vec101);
    fill!(102, unexpected_vec102); fill!(103, unexpected_vec103);
    fill!(104, unexpected_vec104); fill!(105, unexpected_vec105);
    fill!(106, unexpected_vec106); fill!(107, unexpected_vec107);
    fill!(108, unexpected_vec108); fill!(109, unexpected_vec109);
    fill!(110, unexpected_vec110); fill!(111, unexpected_vec111);
    fill!(112, unexpected_vec112); fill!(113, unexpected_vec113);
    fill!(114, unexpected_vec114); fill!(115, unexpected_vec115);
    fill!(116, unexpected_vec116); fill!(117, unexpected_vec117);
    fill!(118, unexpected_vec118); fill!(119, unexpected_vec119);
    fill!(120, unexpected_vec120); fill!(121, unexpected_vec121);
    fill!(122, unexpected_vec122); fill!(123, unexpected_vec123);
    fill!(124, unexpected_vec124); fill!(125, unexpected_vec125);
    fill!(126, unexpected_vec126); fill!(127, unexpected_vec127);
    fill!(128, unexpected_vec128); fill!(129, unexpected_vec129);
    fill!(130, unexpected_vec130); fill!(131, unexpected_vec131);
    fill!(132, unexpected_vec132); fill!(133, unexpected_vec133);
    fill!(134, unexpected_vec134); fill!(135, unexpected_vec135);
    fill!(136, unexpected_vec136); fill!(137, unexpected_vec137);
    fill!(138, unexpected_vec138); fill!(139, unexpected_vec139);
    fill!(140, unexpected_vec140); fill!(141, unexpected_vec141);
    fill!(142, unexpected_vec142); fill!(143, unexpected_vec143);
    fill!(144, unexpected_vec144); fill!(145, unexpected_vec145);
    fill!(146, unexpected_vec146); fill!(147, unexpected_vec147);
    fill!(148, unexpected_vec148); fill!(149, unexpected_vec149);
    fill!(150, unexpected_vec150); fill!(151, unexpected_vec151);
    fill!(152, unexpected_vec152); fill!(153, unexpected_vec153);
    fill!(154, unexpected_vec154); fill!(155, unexpected_vec155);
    fill!(156, unexpected_vec156); fill!(157, unexpected_vec157);
    fill!(158, unexpected_vec158); fill!(159, unexpected_vec159);
    fill!(160, unexpected_vec160); fill!(161, unexpected_vec161);
    fill!(162, unexpected_vec162); fill!(163, unexpected_vec163);
    fill!(164, unexpected_vec164); fill!(165, unexpected_vec165);
    fill!(166, unexpected_vec166); fill!(167, unexpected_vec167);
    fill!(168, unexpected_vec168); fill!(169, unexpected_vec169);
    fill!(170, unexpected_vec170); fill!(171, unexpected_vec171);
    fill!(172, unexpected_vec172); fill!(173, unexpected_vec173);
    fill!(174, unexpected_vec174); fill!(175, unexpected_vec175);
    fill!(176, unexpected_vec176); fill!(177, unexpected_vec177);
    fill!(178, unexpected_vec178); fill!(179, unexpected_vec179);
    fill!(180, unexpected_vec180); fill!(181, unexpected_vec181);
    fill!(182, unexpected_vec182); fill!(183, unexpected_vec183);
    fill!(184, unexpected_vec184); fill!(185, unexpected_vec185);
    fill!(186, unexpected_vec186); fill!(187, unexpected_vec187);
    fill!(188, unexpected_vec188); fill!(189, unexpected_vec189);
    fill!(190, unexpected_vec190); fill!(191, unexpected_vec191);
    fill!(192, unexpected_vec192); fill!(193, unexpected_vec193);
    fill!(194, unexpected_vec194); fill!(195, unexpected_vec195);
    fill!(196, unexpected_vec196); fill!(197, unexpected_vec197);
    fill!(198, unexpected_vec198); fill!(199, unexpected_vec199);
    fill!(200, unexpected_vec200); fill!(201, unexpected_vec201);
    fill!(202, unexpected_vec202); fill!(203, unexpected_vec203);
    fill!(204, unexpected_vec204); fill!(205, unexpected_vec205);
    fill!(206, unexpected_vec206); fill!(207, unexpected_vec207);
    fill!(208, unexpected_vec208); fill!(209, unexpected_vec209);
    fill!(210, unexpected_vec210); fill!(211, unexpected_vec211);
    fill!(212, unexpected_vec212); fill!(213, unexpected_vec213);
    fill!(214, unexpected_vec214); fill!(215, unexpected_vec215);
    fill!(216, unexpected_vec216); fill!(217, unexpected_vec217);
    fill!(218, unexpected_vec218); fill!(219, unexpected_vec219);
    fill!(220, unexpected_vec220); fill!(221, unexpected_vec221);
    fill!(222, unexpected_vec222); fill!(223, unexpected_vec223);
    fill!(224, unexpected_vec224); fill!(225, unexpected_vec225);
    fill!(226, unexpected_vec226); fill!(227, unexpected_vec227);
    fill!(228, unexpected_vec228); fill!(229, unexpected_vec229);
    fill!(230, unexpected_vec230); fill!(231, unexpected_vec231);
    fill!(232, unexpected_vec232); fill!(233, unexpected_vec233);
    fill!(234, unexpected_vec234); fill!(235, unexpected_vec235);
    fill!(236, unexpected_vec236); fill!(237, unexpected_vec237);
    fill!(238, unexpected_vec238); fill!(239, unexpected_vec239);
    fill!(240, unexpected_vec240); fill!(241, unexpected_vec241);
    fill!(242, unexpected_vec242); fill!(243, unexpected_vec243);
    fill!(244, unexpected_vec244); fill!(245, unexpected_vec245);
    fill!(246, unexpected_vec246); fill!(247, unexpected_vec247);
    fill!(248, unexpected_vec248); fill!(249, unexpected_vec249);
    fill!(250, unexpected_vec250); fill!(251, unexpected_vec251);
    fill!(252, unexpected_vec252); fill!(253, unexpected_vec253);
    fill!(254, unexpected_vec254);
}
