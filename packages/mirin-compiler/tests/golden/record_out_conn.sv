module produce (
    input  logic clk,
    input  logic v,
    input  logic [7:0] d,
    output logic downstream__valid,
    output logic [7:0] downstream__data,
    input  logic downstream__ready,
    output logic result
);
    logic accepted;
    assign accepted = downstream__ready;
    assign downstream__valid = v;
    assign downstream__data = d;
    assign result = accepted;
endmodule
