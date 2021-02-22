#![allow(clippy::float_cmp)]

use ::msfs::{
    self,
    legacy::NamedVariable,
    sim_connect::{data_definition, Period, SimConnectRecv, SIMCONNECT_OBJECT_ID_USER},
};

mod athr;
mod lf;
mod pid;
mod rl;

#[data_definition]
#[derive(Debug)]
struct Flight {
    #[name = "AIRSPEED INDICATED"]
    #[unit = "knots"]
    airspeed: f64,
    #[name = "INDICATED ALTITUDE"]
    #[unit = "feet"]
    altitude: f64,
    #[name = "AUTOPILOT AIRSPEED HOLD VAR"]
    #[unit = "knots"]
    airspeed_hold: f64,
    #[name = "AUTOPILOT ALTITUDE LOCK VAR"]
    #[unit = "feet"]
    altitude_lock: f64,
    #[name = "RADIO HEIGHT"]
    #[unit = "feet"]
    radio_height: f64,
    #[name = "AUTOPILOT MASTER"]
    #[unit = "bool"]
    autopilot: bool,
}

#[data_definition]
#[derive(Debug)]
struct Output {
    #[name = "GENERAL ENG THROTTLE LEVER POSITION:1"]
    #[unit = "percent"]
    t1: f64,
    #[name = "GENERAL ENG THROTTLE LEVER POSITION:2"]
    #[unit = "percent"]
    t2: f64,
}

fn mapf(n: f64, in_min: f64, in_max: f64, out_min: f64, out_max: f64) -> f64 {
    (n - in_min) * (out_max - out_min) / (in_max - in_min) + out_min
}

const INC_DELTA: f64 = 16384.0 * (2.5 / 100.0);
const INC_DELTA_SMALL: f64 = 16384.0 * (1.0 / 100.0);
fn nudge(t: &mut f64, d: f64) {
    let n = *t + d;
    // if this change would move the value across 0
    // (negative to positive or positive to negative)
    // then stop at zero, as that is likely what the
    // player actually intended.
    *t = if n.signum() != t.signum() { 0.0 } else { n };
}

#[msfs::standalone_module]
pub async fn module(mut module: msfs::StandaloneModule) -> Result<(), Box<dyn std::error::Error>> {
    let mut sim = module.open_simconnect("ATHR")?;
    let mut athr = athr::AutoThrottle::new();

    let lever_positions = [
        NamedVariable::from("A32NX_3D_THROTTLE_LEVER_POSITION_1"),
        NamedVariable::from("A32NX_3D_THROTTLE_LEVER_POSITION_2"),
    ];
    let athr_armed_out = NamedVariable::from("A32NX_AUTOTHROTTLE_ARMED");

    let revtog_id = sim.map_client_event_to_sim_event("THROTTLE_REVERSE_THRUST_TOGGLE", true)?;
    let revhold_id = sim.map_client_event_to_sim_event("THROTTLE_REVERSE_THRUST_HOLD", true)?;

    let tset_id = sim.map_client_event_to_sim_event("THROTTLE_SET", true)?;
    let t1set_id = sim.map_client_event_to_sim_event("THROTTLE1_SET", true)?;
    let t2set_id = sim.map_client_event_to_sim_event("THROTTLE2_SET", true)?;

    let atset_id = sim.map_client_event_to_sim_event("AXIS_THROTTLE_SET", true)?;
    let at1set_id = sim.map_client_event_to_sim_event("AXIS_THROTTLE1_SET", true)?;
    let at2set_id = sim.map_client_event_to_sim_event("AXIS_THROTTLE2_SET", true)?;

    let exset_id = sim.map_client_event_to_sim_event("THROTTLE_AXIS_SET_EX1", true)?;
    let ex1set_id = sim.map_client_event_to_sim_event("THROTTLE1_AXIS_SET_EX1", true)?;
    let ex2set_id = sim.map_client_event_to_sim_event("THROTTLE2_AXIS_SET_EX1", true)?;

    let thrinc_id = sim.map_client_event_to_sim_event("THROTTLE_INCR", true)?;
    let thrdec_id = sim.map_client_event_to_sim_event("THROTTLE_DECR", true)?;
    let thrincs_id = sim.map_client_event_to_sim_event("THROTTLE_INCR_SMALL", true)?;
    let thrdecs_id = sim.map_client_event_to_sim_event("THROTTLE_DECR_SMALL", true)?;

    let thr1inc_id = sim.map_client_event_to_sim_event("THROTTLE1_INCR", true)?;
    let thr1dec_id = sim.map_client_event_to_sim_event("THROTTLE1_DECR", true)?;
    let thr1incs_id = sim.map_client_event_to_sim_event("THROTTLE1_INCR_SMALL", true)?;
    let thr1decs_id = sim.map_client_event_to_sim_event("THROTTLE1_DECR_SMALL", true)?;

    let thr2inc_id = sim.map_client_event_to_sim_event("THROTTLE2_INCR", true)?;
    let thr2dec_id = sim.map_client_event_to_sim_event("THROTTLE2_DECR", true)?;
    let thr2incs_id = sim.map_client_event_to_sim_event("THROTTLE2_INCR_SMALL", true)?;
    let thr2decs_id = sim.map_client_event_to_sim_event("THROTTLE2_DECR_SMALL", true)?;

    let athrpb_id = sim.map_client_event_to_sim_event("AUTO_THROTTLE_ARM", true)?;
    let inst_id = sim.map_client_event_to_sim_event("A32NX.ATHR_INSTINCTIVE_DISCONNECT", true)?;

    sim.request_data_on_sim_object::<Flight>(0, SIMCONNECT_OBJECT_ID_USER, Period::SimFrame)?;

    let mut reverse_toggle = false;
    let mut reverse_hold = false;
    let mut t1 = 0.0;
    let mut t2 = 0.0;

    let mut last_t = std::time::Instant::now();

    macro_rules! calc {
        ($v:expr) => {
            $v as std::os::raw::c_long as f64
        };
    }

    let mut last_altitude_lock = 0.0;

    while let Some(recv) = module.next_event().await {
        match recv {
            SimConnectRecv::Event(event) => match event.id() {
                x if x == revtog_id => {
                    reverse_toggle = !reverse_toggle;
                }
                x if x == revhold_id => {
                    reverse_hold = event.data() == 1;
                }
                x if x == tset_id => {
                    let data = calc!(event.data());
                    t1 = data;
                    t2 = data;
                }
                x if x == t1set_id => {
                    t1 = calc!(event.data());
                }
                x if x == t2set_id => {
                    t2 = calc!(event.data());
                }
                x if x == atset_id => {
                    let data = calc!(event.data());
                    t1 = data;
                    t2 = data;
                }
                x if x == at1set_id => {
                    t1 = calc!(event.data());
                }
                x if x == at2set_id => {
                    t2 = calc!(event.data());
                }
                x if x == exset_id => {
                    let data = calc!(event.data());
                    t1 = data;
                    t2 = data;
                }
                x if x == ex1set_id => {
                    t1 = calc!(event.data());
                }
                x if x == ex2set_id => {
                    t2 = calc!(event.data());
                }
                x if x == thrinc_id => {
                    nudge(&mut t1, INC_DELTA);
                    nudge(&mut t2, INC_DELTA);
                }
                x if x == thrdec_id => {
                    nudge(&mut t1, -INC_DELTA);
                    nudge(&mut t2, -INC_DELTA);
                }
                x if x == thrincs_id => {
                    nudge(&mut t1, INC_DELTA_SMALL);
                    nudge(&mut t2, INC_DELTA_SMALL);
                }
                x if x == thrdecs_id => {
                    nudge(&mut t1, -INC_DELTA_SMALL);
                    nudge(&mut t2, -INC_DELTA_SMALL);
                }
                x if x == thr1inc_id => nudge(&mut t1, INC_DELTA),
                x if x == thr1dec_id => nudge(&mut t1, -INC_DELTA),
                x if x == thr1incs_id => nudge(&mut t1, INC_DELTA_SMALL),
                x if x == thr1decs_id => nudge(&mut t1, -INC_DELTA_SMALL),
                x if x == thr2inc_id => nudge(&mut t2, INC_DELTA),
                x if x == thr2dec_id => nudge(&mut t2, -INC_DELTA),
                x if x == thr2incs_id => nudge(&mut t2, INC_DELTA_SMALL),
                x if x == thr2decs_id => nudge(&mut t2, -INC_DELTA_SMALL),
                x if x == athrpb_id => {
                    athr.input().pushbutton = true;
                }
                x if x == inst_id => {
                    athr.input().instinctive_disconnect = event.data() == 1;
                }
                _ => unreachable!(),
            },
            SimConnectRecv::SimObjectData(data) => match data.id() {
                0 => {
                    let data = data.into::<Flight>(&sim).unwrap();
                    let input = athr.input();
                    input.airspeed = data.airspeed;
                    input.airspeed_target = data.airspeed_hold;
                    // input.vls = (L:A32NX_SPEEDS_VLS, knots);
                    // input.alpha_floor = ??;
                    input.radio_height = data.radio_height;

                    if data.autopilot && last_altitude_lock != data.altitude_lock {
                        last_altitude_lock = data.altitude_lock;
                        input.mode = if data.altitude_lock > data.altitude {
                            athr::Mode::ThrustClimb
                        } else {
                            athr::Mode::ThrustDescent
                        };
                    }
                    if !data.autopilot || (data.altitude_lock - data.altitude).abs() < 1000.0 {
                        input.mode = athr::Mode::Speed;
                    }
                }
                _ => unreachable!(),
            },
            _ => {}
        }

        let map = |t| {
            if reverse_hold || reverse_toggle {
                athr::TLA::get(mapf(t, -16384.0, 16384.0, -6.0, -20.0))
            } else {
                athr::TLA::get(mapf(t, -16384.0, 16384.0, 0.0, 100.0) * 0.45)
            }
        };

        athr.input().throttles = [map(t1), map(t2)];

        let dt = last_t.elapsed();
        last_t = std::time::Instant::now();

        athr.update(dt);

        // clear momentary input
        athr.input().pushbutton = false;

        {
            let output = athr.output();
            let odata = Output {
                t1: output.commanded[0],
                t2: output.commanded[1],
            };

            let map = |t| {
                if reverse_hold || reverse_toggle {
                    mapf(t, -16384.0, 16384.0, 0.0, 25.0)
                } else {
                    mapf(t, -16384.0, 16384.0, 25.0, 100.0)
                }
            };
            println!("map({}) = {}", t1, map(t1));
            lever_positions[0].set_value(map(t1));
            lever_positions[1].set_value(map(t2));

            athr_armed_out.set_value(output.armed);

            sim.set_data_on_sim_object(SIMCONNECT_OBJECT_ID_USER, &odata)?;
        }
    }

    Ok(())
}
